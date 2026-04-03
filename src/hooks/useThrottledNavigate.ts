import { useNavigate } from "react-router";
import { useRef, useCallback } from "react";
import { throttle } from "radash";

export const useThrottledNavigate = (delay: number = 500) => {
    const navigate = useNavigate();
    const debouncedRef = useRef<ReturnType<typeof throttle> | undefined>(undefined);

    return useCallback(
        (path: string) => {
            if (!debouncedRef.current) {
                debouncedRef.current = throttle(
                    { interval: delay },
                    (p: string) => navigate(p)
                );
            }
            debouncedRef.current(path);
        },
        [navigate, delay]
    );
};
