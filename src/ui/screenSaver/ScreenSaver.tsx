import classnames from "classnames";

import "./screenSaver.css";
import screenSaverVideoMP4 from "./screensaver_looped.mp4";
import screenSaverVideoWEBM from "./screensaver_looped.webm";
import statueSVG from "./statue.svg";
import handSVG from "./hand.svg";

export interface ScreenSaverProps {
    shouldShow: boolean;
}

export function ScreenSaver({ shouldShow }: ScreenSaverProps) {
    return (
        <div className={classnames("screen-saver", { visible: shouldShow })}>
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