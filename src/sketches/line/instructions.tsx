import * as classnames from "classnames";
import * as React from "react";

export interface InstructionsState {
    leapMotionControllerValid: boolean;
    lastRenderedFrame: number;
    globalFrame: number;
}

export class Instructions extends React.Component<{}, InstructionsState> {
    // tslint:disable-next-line:member-access
    state = {
        leapMotionControllerValid: false,
        globalFrame: 0,
        lastRenderedFrame: -Infinity,
    };

public render() {
	const numSecondsToShowInstructions = 10;
	const shouldShow = !(this.state.globalFrame - this.state.lastRenderedFrame < 60 * numSecondsToShowInstructions) && this.state.leapMotionControllerValid;
   
	// Inline styles
	const styles = {
		container: {
			pointerEvents: "none",
		} as React.CSSProperties,
		video: {
			position: "absolute",
			top: 0,
			left: 0,
			width: "100%",
			height: "100%",
			objectFit: "cover",
			margin: 0,
			padding: 0,
		} as React.CSSProperties,
	};

	return (
		<>
			{/* Inline CSS for classes, because css compilation is broken */}
			<style>
				{`
					.line-instructions {
						opacity: 0;
						transition: opacity 500ms ease-in;
					}
					.line-instructions.visible {
						opacity: 1;
					}
				`}
			</style>
			<div
				className={classnames("line-instructions", { visible: shouldShow })}
				style={styles.container}
			>
				<video autoPlay muted loop style={styles.video}>
					<source src="/assets/images/capture.mp4" type="video/mp4" />
					Your browser does not support the video tag.
				</video>
			</div>
		</>
	);


}

    public setGlobalFrame(f: number) {
        this.setState({ globalFrame: f });
    }

    public setLastRenderedFrame(lastRenderedFrame: number) {
		console.log( lastRenderedFrame );
        this.setState({ lastRenderedFrame });
    }

    public setLeapMotionControllerValid(valid: boolean) {
        this.setState({ leapMotionControllerValid: valid });
    }
}