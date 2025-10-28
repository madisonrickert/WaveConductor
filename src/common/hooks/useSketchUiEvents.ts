import { useEffect } from "react";
import { UI_EVENTS, UIEventReciever } from "@/sketch";
// @todo Eliminate jQuery dependency here
import $ from "jquery";

/**
 * Attaches/detaches the sketch's UI event handlers to the provided DOM target.
 */
export function useSketchUiEvents(events: UIEventReciever, target: HTMLElement) {
  useEffect(() => {
    const $target = $(target);
    const entries = Object.entries(events) as Array<
      [keyof typeof UI_EVENTS, JQuery.EventHandler<HTMLElement> | undefined]
    >;

    entries.forEach(([eventName, handler]) => {
      if (handler) {
        $target.on(eventName, handler);
      }
    });

    return () => {
      entries.forEach(([eventName, handler]) => {
        if (handler) {
          $target.off(eventName, handler);
        }
      });
    };
  }, [events, target]);
}