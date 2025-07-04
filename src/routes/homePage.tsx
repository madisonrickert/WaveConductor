import React from "react";
import { Link } from "react-router";
import { FaPlay } from "react-icons/fa";

export function HomePage() {
    function renderHighlight(name: string, imageUrl: string, linkUrl?: string) {
        const Wrapper: any = linkUrl ? "a" : Link;
        const wrapperProps = linkUrl
            ? { className: "work-highlight-link", href: linkUrl, target: "_blank" }
            : { className: "work-highlight-link", to: `/${name.toLowerCase()}` };

        return (
            <figure className="work-highlight work-grid-item" key={name}>
                <Wrapper {...wrapperProps}>
                    <div className="work-highlight-image">
                        <img className="full-width" src={imageUrl} />
                        <span className="work-highlight-name">{name}</span>
                        <div className="work-highlight-sheen sheen-on-hover">
                            <FaPlay />
                        </div>
                    </div>
                </Wrapper>
            </figure>
        );
    }

    return (
        <div className="homepage">
            <main className="content">
                <section className="content-section work work-grid" id="work">
                    {renderHighlight("Cymatics", "/assets/images/cymatics5_cropped.jpg")}
                    {renderHighlight("Line", "/assets/images/gravity4_cropped.jpg")}
                    {renderHighlight("Flame", "/assets/images/flame.jpg")}
                    {renderHighlight("Dots", "/assets/images/dots2.jpg")}
                    {renderHighlight("Waves", "/assets/images/waves2.jpg")}
                    {renderHighlight("Mito", "/assets/images/mito_cover.png", 'https://hellochar.github.io/mito/#/')}
                </section>
            </main>
            <footer className="page-footer">
                <div className="copyright">
                    <h3>Credits & Copyright:</h3>
                    <ul>
                        <li><a href="https://github.com/hellochar">Xiaohan Zhang</a></li>
                        <li><a href="https://joshrickert.com">Madison Rickert</a></li>
                        <li><a href="https://lovetech.org">Rich Trapani / LoveTech</a></li>
                    </ul>
                </div>
            </footer>
        </div>
    );
}

