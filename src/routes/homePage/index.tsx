import React from "react";
import { Link } from "react-router";
import { FaPlay } from "react-icons/fa";

import cymaticsImg from "@/sketches/cymatics/screenshots/cymatics5_cropped.jpg";
import lineImg from "@/sketches/line/screenshots/gravity4_cropped.jpg";
import flameImg from "@/sketches/flame/screenshots/flame.jpg";
import dotsImg from "@/sketches/dots/screenshots/dots2.jpg";
import wavesImg from "@/sketches/waves/screenshots/waves2.jpg";

import "./homePage.scss";

export function HomePage() {
    function renderHighlight(name: string, imageUrl: string, linkUrl?: string) {
        const Wrapper: React.ElementType = linkUrl ? "a" : Link;
        const wrapperProps = linkUrl
            ? { className: "work-highlight-link", href: linkUrl, target: "_blank" }
            : { className: "work-highlight-link", to: `/${name.toLowerCase()}` };

        return (
            <figure className="work-highlight work-grid-item" key={name}>
                <Wrapper {...wrapperProps}>
                    <div className="work-highlight-image">
                        <img className="full-width" src={imageUrl} alt={name} />
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
                    {renderHighlight("Cymatics", cymaticsImg)}
                    {renderHighlight("Line", lineImg)}
                    {renderHighlight("Flame", flameImg)}
                    {renderHighlight("Dots", dotsImg)}
                    {renderHighlight("Waves", wavesImg)}
                    <div className="work-grid-item credits-block">
                        <div className="credits-content">
                            <h2>CharGallery</h2>
                            <p className="credits-attribution">based on <a href="https://github.com/hellochar/hellochar.com">hellochar</a> by <a href="https://github.com/hellochar">Xiaohan Zhang</a></p>
                            <ul>
                                <li><a href="https://madisonrickert.com">Madison Rickert</a></li>
                                <li><a href="https://lovetech.org">Rich Trapani | LoveTech</a></li>
                            </ul>
                            <Link to="/licenses" className="licenses-link">Open Source Licenses</Link>
                        </div>
                    </div>
                </section>
            </main>
        </div>
    );
}

