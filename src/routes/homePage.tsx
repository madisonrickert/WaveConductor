import React from "react";
import { RouteComponentProps } from "react-router";
import { Link } from "react-router-dom";
import Hero from "./hero";
import { HistorySection } from "./history";
import { ShrinkingHeader } from "./shrinkingHeader";

import { FaPlay } from "react-icons/fa";

export class HomePage extends React.Component<RouteComponentProps<void>, object> {
    public render() {
        return (
            <div className="homepage">
                { this.renderHeader() }
                <Hero />
                { this.renderContent() }
                { this.renderFooter() }
            </div>
        );
    }

    public componentDidMount() {
        const hash = this.props.location.hash;
        const element = document.getElementById(hash);
        if (element != null) {
            element.scrollIntoView();
        }
    }

    private renderHeader() {
        return <ShrinkingHeader />;
    }

    private renderContent() {
        return (
            <main className="content">
                { this.renderWork() }
                <HistorySection />
            </main>
        );
    }

    private renderFooter() {
        return (
            <footer className="page-footer">
                <a href="#contact">
                    <div className="get-in-touch">Get in touch</div>
                </a>
                <div className="copyright">
                    &copy; 2013 - present Xiaohan Zhang
                </div>
            </footer>
        );
    }

    private renderWork() {
        return (
            <section className="content-section work" id="work">
                { this.renderHighlight("Mito", "/assets/images/mito_cover.png", 'https://hellochar.github.io/mito/#/') }
                { this.renderHighlight("Flame", "/assets/images/flame.jpg") }
                { this.renderHighlight("Line", "/assets/images/gravity4_cropped.jpg") }
                { this.renderHighlight("Dots", "/assets/images/dots2.jpg") }
                { this.renderHighlight("Waves", "/assets/images/waves2.jpg") }
                { this.renderHighlight("Cymatics", "/assets/images/cymatics5_cropped.jpg") }
            </section>
        );
    }

    private renderHighlight(name: string, imageUrl: string, linkUrl?: string) {
        const hasCustomURL = linkUrl != null;
        let innerEl: React.JSX.Element;
        if (hasCustomURL) {
            innerEl = (
                <>
                <figcaption>
                    <a className="work-highlight-name" href={linkUrl} target="_blank">{name}</a>
                </figcaption>
                    <a href={linkUrl} target="_blank">
                        <div className="work-highlight-image">
                            <img className="full-width" src={imageUrl} />
                            <div className="work-highlight-sheen sheen-on-hover">
                                <FaPlay />
                            </div>
                        </div>
                    </a>
                </>
            )
        } else {
            linkUrl = `/${name.toLowerCase()}`;
            innerEl = (
                <>
                <figcaption>
                    <Link className="work-highlight-name" to={linkUrl}>{name}</Link>
                </figcaption>
                <Link to={linkUrl}>
                    <div className="work-highlight-image">
                        <img className="full-width" src={imageUrl} />
                        <div className="work-highlight-sheen sheen-on-hover">
                            <FaPlay />
                        </div>
                    </div>
                </Link>
</>
            );
        }
        return (
            <figure className="work-highlight">
                {innerEl}
            </figure>
        );
    }
}
