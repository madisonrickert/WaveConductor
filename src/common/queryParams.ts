import queryString from "query-string";

/**
 * Parse query parameters from the current URL.
 * Works with both standard URLs (?key=val) and hash-based URLs (#/path?key=val).
 */
export function getQueryParams<T extends Record<string, unknown>>(
  options?: queryString.ParseOptions
): T {
  // Hash-based: extract query string from after '?' in the hash
  const hash = location.hash;
  const hashQueryIndex = hash.indexOf("?");
  if (hashQueryIndex !== -1) {
    return queryString.parse(hash.substring(hashQueryIndex), options) as T;
  }

  // Standard: use location.search
  return queryString.parse(location.search, options) as T;
}

/**
 * Get a single query parameter as a string, or return a fallback.
 */
export function getQueryParam(key: string, fallback: string = ""): string {
  const params = getQueryParams();
  const value = params[key];
  return typeof value === "string" ? value : fallback;
}

/**
 * Update the URL's query parameters (hash-compatible).
 * Uses replaceState so it doesn't create a history entry.
 */
export function setQueryParams(params: Record<string, string>) {
  const search = queryString.stringify(params);
  const hashPath = location.hash.split("?")[0] || "#/";
  const newHash = search ? `${hashPath}?${search}` : hashPath;
  window.history.replaceState({}, "", `${location.pathname}${newHash}`);
}
