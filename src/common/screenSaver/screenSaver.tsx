import classnames from "classnames";
import React from "react";

import "./screenSaver.css";
import screenSaverVideoMP4 from "./capture.mp4";
import screenSaverVideoWEBM from "./capture.webm";
import statueSVG from "./statue.svg";
import handSVG from "./hand.svg";

export interface ScreenSaverProps {
    shouldShow: boolean;
}

export class ScreenSaver extends React.Component<ScreenSaverProps> {
    public render() {
        return (
            <div className={classnames("screen-saver", { visible: this.props.shouldShow })}>
                <video autoPlay muted loop className="video">
                    <source src={screenSaverVideoMP4} type="video/mp4" />
                    <source src={screenSaverVideoWEBM} type="video/webm" />
                    Your browser does not support the video tag.
                </video>
                <img src={statueSVG} alt="Statue" className="statue graphic" />
                <img src={handSVG} alt="Left Hand" className="hand graphic" />
                <img src={handSVG} alt="Right Hand" className="hand hand-right graphic" />
            </div>
        );
    }
}