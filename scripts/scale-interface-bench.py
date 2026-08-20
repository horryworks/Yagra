#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# scale-interface-bench.py — what the interface-threshold candidate query costs at fleet scale
# (ADR-076 increment 6).
#
# The lab cannot answer this. Six nodes and 2,558 ports put two ports above the query floor, so the
# whole cost model is invisible there — `yagra_interface_util_tracked` reads 2 and stays 2. This
# seeds synthetic interface counters into VictoriaMetrics and runs the REAL query shape against
# them, with and without the node selector, so the difference is a measurement rather than an
# argument.
#
# Everything it writes goes under SHADOW metric names (`bench_if_*`). Yagra reads neither, so a
# running deployment is untouched — verified by `grep -r bench_if crates/ web/src/` finding nothing.
# `--clean` deletes them again; run it when you are done, or the series linger until retention.
#
#   seed    add synthetic ports          scale-interface-bench.py seed --ports 400000
#   bench   time the candidate query     scale-interface-bench.py bench --scope 200
#   clean   delete the synthetic series  scale-interface-bench.py clean
#
#   --vm         VictoriaMetrics base URL (default http://127.0.0.1:8428)
#   --ports      how many ports to seed (seed only; 48 ports per synthetic node)
#   --scope      how many nodes to name in the selector; 0 = fleet-wide (bench only)
#   --repeat     how many times to time each query (default 3)
#
# On the test server VictoriaMetrics publishes no host port, so point --vm at the container:
#   VM=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' \
#        yagra-victoriametrics-1)
#   python3 scripts/scale-interface-bench.py seed --vm "http://$VM:8428" --ports 400000
#
# Measured 2026-08-20 on that box (Celeron J1900, 4 cores — absolute numbers are pessimistic; the
# shape and the ratios are what transfer):
#
#   ports    floor=0        floor=900Mbps (0 rows)   response
#   2,000     55 ms          46 ms                    0.22 MB
#   20,000     1.4 s        360 ms                    2.2 MB
#   100,000    5.1 s          2.3 s                   11.0 MB
#   400,000   11.6-39 s       8.3-36 s                43.9 MB
#
# The headline is that `seriesFetched` is IDENTICAL at every floor: the floor removes rows from the
# answer, never series from the scan. That is why increment 6c added the node selector.

import argparse
import json
import sys
import time
import urllib.parse
import urllib.request

PORTS_PER_NODE = 48
STEP_MS = 60_000  # a 60s poll, like the real one
SPAN_S = 2400     # covers the 1800s instant lookback plus the 300s rate window, with slack
NPOINTS = SPAN_S // 60
IN, OUT = "bench_if_in_octets", "bench_if_out_octets"


def node_id(i):
    """A UUID-shaped, obviously-synthetic node label — same 36-byte width as a real one, so the
    selector-length arithmetic this measures matches production."""
    return "bench%03d-%04d-0000-0000-000000000000" % (i // 10000, i % 10000)


def post(vm, path, body, timeout=600):
    req = urllib.request.Request(vm + path, data=body,
                                 headers={"Content-Type": "application/json"})
    return urllib.request.urlopen(req, timeout=timeout).read()


def query(vm, expr, timeout=900):
    body = urllib.parse.urlencode({"query": expr}).encode()
    t0 = time.time()
    try:
        raw = urllib.request.urlopen(vm + "/api/v1/query", data=body, timeout=timeout).read()
    except urllib.error.HTTPError as e:
        # A refusal IS a result worth printing: core reads any non-2xx as "the store did not
        # answer" and skips the whole tick, so alerting simply stops for that minute.
        return {"HTTP_ERROR": e.code, "body": e.read()[:300].decode("utf-8", "replace")}, 0, \
            (time.time() - t0) * 1000
    return json.loads(raw), len(raw), (time.time() - t0) * 1000


def cmd_seed(a):
    now = int(time.time() * 1000) // STEP_MS * STEP_MS
    ts = [now - (NPOINTS - 1 - k) * STEP_MS for k in range(NPOINTS)]
    buf, sent, t0 = [], 0, time.time()
    for i in range(a.ports):
        node, ifx = node_id(i // PORTS_PER_NODE), str(i % PORTS_PER_NODE + 1)
        # 1 port in 100 is busy (~200 Mbit/s); the rest near-idle (~0.4 Mbit/s). Bytes/sec.
        rate = 25_000_000 if i % 100 == 0 else 50_000
        for name in (IN, OUT):
            base = 1_000_000_000 + i * 7919
            buf.append(json.dumps(
                {"metric": {"__name__": name, "node": node, "ifindex": ifx},
                 "values": [base + rate * 60 * k for k in range(NPOINTS)], "timestamps": ts},
                separators=(",", ":")))
        if len(buf) >= 4000:
            post(a.vm, "/api/v1/import", ("\n".join(buf) + "\n").encode())
            sent += len(buf)
            buf = []
            print("  ... %d series in %.0fs" % (sent, time.time() - t0), flush=True)
    if buf:
        post(a.vm, "/api/v1/import", ("\n".join(buf) + "\n").encode())
        sent += len(buf)
    urllib.request.urlopen(a.vm + "/internal/force_flush", timeout=180).read()
    print("seeded %d series (%d ports x 2 metrics, %d points each) in %.0fs"
          % (sent, a.ports, NPOINTS, time.time() - t0))


def cmd_bench(a):
    # 🚨 The count is a LABEL, never a gate. It used to `sys.exit("run seed first")` when it came
    # back empty — and at 400k ports `count()` itself exceeds VictoriaMetrics' default
    # `-search.maxQueryDuration=30s` and returns 422, so the tool announced "no data" about a store
    # holding 800,000 series and measured nothing. Report what happened and carry on.
    d, _, _ = query(a.vm, "count(last_over_time(%s[1800s]))" % IN)
    if "HTTP_ERROR" in d:
        total = "unknown (count query: HTTP %s — %s)" % (d["HTTP_ERROR"], d["body"].strip()[:120])
    else:
        res = d.get("data", {}).get("result", [])
        total = int(float(res[0]["value"][1])) if res else 0
        if total == 0:
            sys.exit("no %s series — run `seed` first" % IN)

    # The selector, built exactly as `store.rs::candidate_selectors` builds it, so what is timed
    # here is the query the evaluator actually issues.
    if a.scope:
        ids = "|".join(node_id(i) for i in range(a.scope))
        sel, label = '{node=~"%s"}' % ids, "scoped to %d nodes" % a.scope
    else:
        sel, label = "", "fleet-wide"

    print("=== ports=%s, %s ===" % (total, label), flush=True)
    for floor in (0, 57_600, 900_000_000):
        # `interface_candidates_query(InBps, floor, sel)`, verbatim.
        expr = ("max by (node,ifindex) (last_over_time((rate(%s%s[300s]) * 8)[1800s:300s])) >= %d"
                % (IN, sel, floor))
        if len(expr) >= 16_384:
            print("  floor=%-11d SKIPPED: query is %d bytes, over -search.maxQueryLen"
                  % (floor, len(expr)))
            continue
        for _ in range(a.repeat):
            d, nbytes, wall = query(a.vm, expr)
            if "HTTP_ERROR" in d:
                print("  floor=%-11d HTTP %s after %dms -- %s"
                      % (floor, d["HTTP_ERROR"], wall, d["body"].strip()), flush=True)
                continue
            st = d.get("stats", {})
            print("  floor=%-11d rows=%-8d bytes=%-10d fetched=%-9s vmMs=%-7s wallMs=%d"
                  % (floor, len(d["data"]["result"]), nbytes,
                     st.get("seriesFetched", "?"), st.get("executionTimeMsec", "?"), wall),
                  flush=True)


def cmd_clean(a):
    for name in (IN, OUT):
        u = a.vm + "/api/v1/admin/tsdb/delete_series?match[]=" + name
        code = urllib.request.urlopen(u, timeout=300).status
        print("delete %s -> HTTP %s" % (name, code))
    d, _, _ = query(a.vm, "count(last_over_time(%s[1800s]))" % IN)
    print("remaining synthetic series: %s" % (d.get("data", {}).get("result", []) or 0))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("command", choices=["seed", "bench", "clean"])
    p.add_argument("--vm", default="http://127.0.0.1:8428")
    p.add_argument("--ports", type=int, default=100_000)
    p.add_argument("--scope", type=int, default=0)
    p.add_argument("--repeat", type=int, default=3)
    a = p.parse_args()
    {"seed": cmd_seed, "bench": cmd_bench, "clean": cmd_clean}[a.command](a)


if __name__ == "__main__":
    main()
