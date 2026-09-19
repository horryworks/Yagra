// SPDX-License-Identifier: AGPL-3.0-only
// The Meraki Dashboard API regions the "Add organization" dialog offers, as data.
//
// This list has a second copy in another language: `yagra_common::is_meraki_api_host` is the
// allow-list the backend checks a base URL against before it sends the API key anywhere. The two
// drifted once — this list offered Canada while the allow-list refused `api.meraki.ca`, so picking
// that region answered `400 invalid_base_url` every time (ADR-164). A Rust test now reads THIS file
// and runs every `https://` URL in it through the function the endpoint runs
// (`api/meraki.rs::every_region_the_webui_offers_passes_the_base_url_allowlist`), so a region
// added here without its host being allow-listed fails the backend suite.
//
// ⚠️ That test finds the URLs by looking for quoted strings starting with `https://`. Keep each
// base URL a plain string literal in this file — a URL assembled from parts would go unchecked.
//
// Kept in a `.ts` so tests can iterate it (Vitest only runs `src/**/*.test.ts`); it sat inline in
// `MerakiIntegrationPage.tsx`, which `ui-conventions.md` named as a list to fold into a source.

/** Every offered region, the default first. `key` is the tail of its `system:meraki.regions.*`
 *  label; `base_url` is the technical endpoint and is never translated. */
export const MERAKI_REGIONS = [
  { key: 'global', base_url: 'https://api.meraki.com' },
  { key: 'canada', base_url: 'https://api.meraki.ca' },
  { key: 'china', base_url: 'https://api.meraki.cn' },
  { key: 'usGov', base_url: 'https://api.gov-meraki.com' },
] as const;

export type MerakiRegionKey = (typeof MERAKI_REGIONS)[number]['key'];

/** The keys alone, for the coverage test that demands a label for each in both locales. */
export const MERAKI_REGION_KEYS = MERAKI_REGIONS.map((r) => r.key);

/** What the dialog starts on: the global shard, which is also the backend's default when a request
 *  names no base URL. */
export const DEFAULT_MERAKI_BASE_URL = MERAKI_REGIONS[0].base_url;
