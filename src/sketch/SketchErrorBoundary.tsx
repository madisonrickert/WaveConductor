import { Component, ReactNode } from "react";

interface SketchErrorBoundaryState {
    error: Error | null;
}

/**
 * Catches render errors from a sketch's React overlay (e.g. FlameNameInput)
 * and displays a fallback message instead of crashing the entire app.
 */
export class SketchErrorBoundary extends Component<{ children: ReactNode }, SketchErrorBoundaryState> {
    state: SketchErrorBoundaryState = { error: null };

    static getDerivedStateFromError(error: Error) {
        return { error };
    }

    render() {
        if (this.state.error) {
            return (
                <div className="sketch-error">
                    <p>Something went wrong rendering this sketch.</p>
                    <pre>{this.state.error.message}</pre>
                </div>
            );
        }
        return this.props.children;
    }
}
