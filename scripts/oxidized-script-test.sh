#!/usr/bin/env bash
# Runs the config save script (ADR-222) inside the oxidized image the composition pins, against a
# fake device. The image is read from docker-compose.deploy.yml, so moving the pin is what makes
# this test the new version - the script leans on oxidized internals an upgrade can move.
#
# Needs Docker. Prints PASS/FAIL per case and exits non-zero on any failure.
set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE=$(awk '$1 == "image:" && $2 ~ /^oxidized\/oxidized:/ { print $2; exit }' docker-compose.deploy.yml)
if [ -z "$IMAGE" ]; then
  echo "no oxidized image in docker-compose.deploy.yml" >&2
  exit 2
fi
echo "oxidized image: $IMAGE"
docker run --rm --entrypoint ruby \
  -v "$PWD/crates/yagra-core/src/oxidized/yagra-save.rb:/t/yagra-save.rb:ro" \
  -v "$PWD/scripts/oxidized-script-test:/t/harness:ro" \
  "$IMAGE" /t/harness/run.rb /t/yagra-save.rb
