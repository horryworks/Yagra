// Flow-tab drill-down filter helpers (ADR-031). Kept pure (no React) so the click-to-filter and
// AND-combination logic is unit-testable without rendering the whole Flow tab. The raw filter
// inputs (protocol / destination port / peer IP) are the same values the typed filter bar drives
// and that clicking a Top-N row populates; `buildFlowFilters` turns them into the typed set sent to
// the flow API, where two or more set values are ANDed.

import type { FlowFilters } from '../../types/api';

/** The raw (string) filter inputs shared by the typed bar and click-to-filter. */
export interface FlowFilterInputs {
  proto: string;
  port: string;
  peer: string;
}

/**
 * Assemble the typed filter set for the flow API from the raw inputs. Blank / invalid values are
 * omitted (so an unset filter never reaches the query); when two or more survive, the API ANDs them.
 */
export function buildFlowFilters({ proto, port, peer }: FlowFilterInputs): FlowFilters {
  const filters: FlowFilters = {};
  const protoNum = proto ? Number(proto) : NaN;
  if (Number.isInteger(protoNum)) filters.proto = protoNum;
  const portNum = port ? Number(port) : NaN;
  if (Number.isInteger(portNum) && portNum >= 0 && portNum <= 65535) filters.port = portNum;
  if (peer.trim()) filters.peer = peer.trim();
  return filters;
}

/**
 * Click-to-filter toggle: applying a value sets it, but clicking the row that is already the active
 * filter clears it — so a second click on the same talker/protocol/port removes that filter.
 */
export function toggleFilterValue(current: string, clicked: string): string {
  return current === clicked ? '' : clicked;
}
