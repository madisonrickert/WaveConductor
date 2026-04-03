/**
 * Central device capability detection.
 *
 * `isTouchDevice` is true when the primary pointer is coarse (finger/stylus),
 * which reliably identifies phones and tablets. Prefer this over `ontouchstart`
 * checks — laptops with touchscreens still report `pointer: fine`.
 */
export const isTouchDevice =
    typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches;
