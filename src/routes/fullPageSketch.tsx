import classnames from "classnames";
import queryString from "query-string";
import React from "react";
import { Link } from "react-router-dom";

import { SketchConstructor } from "../sketch";
import { SketchComponent } from "../sketchComponent";

export interface ISketchRouteProps {
    sketchClass: SketchConstructor;

    isKiosk?: boolean;
}

export class FullPageSketch extends React.Component<ISketchRouteProps, object> {
    public render() {
        const { isKiosk } = this.props;
        const isPresentationMode = !!queryString.parse(location.search).presentationMode;
        const classes = classnames("full-page-sketch", { "presentation-mode": isPresentationMode, "kiosk-mode": isKiosk });
        return (
            <div className={classes} ref={this.handleDivRef}>
                { !isKiosk ? <Link className="back-button" to="/">&#10094;</Link> : null }
                <SketchComponent sketchClass={this.props.sketchClass} />
            </div>
        );
    }
    private handleDivRef = (div: HTMLDivElement | null) => {
        if (div != null) {
            // this.requestFullscreen(div);
        } else {
            this.exitFullscreen();
        }
    }

    private requestFullscreen(ref: HTMLElement) {
        if (ref.requestFullscreen) {
            ref.requestFullscreen();
        } else {
            console.warn("Fullscreen API is not supported in this browser.");
        }
    }

    private exitFullscreen() {
        if (document.exitFullscreen) {
            document.exitFullscreen();
        } else {
            console.warn("Fullscreen API is not supported in this browser.");
        }
    }
}
