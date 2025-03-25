import classnames from "classnames";
import React from "react";

export interface CurtainState {
    closed?: boolean;
}

export class Curtain extends React.PureComponent<object, CurtainState> {
    state = {
        closed: false,
    };

    render() {
        const className = classnames("curtain", { closed: this.state.closed });
        return <div className={className}></div>;
    }
}
