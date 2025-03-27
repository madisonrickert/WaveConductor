Development
===========

Install [nvm](https://github.com/nvm-sh/nvm?tab=readme-ov-file#installing-and-updating). Run `rvm use` to download and activate the correct node version.

Install Leap Motion Software (see below).

Install global dependencies. `npm i -g yarn`.

Install local dependencies. `yarn install`.


Run `yarn start` to start Vite dev server.

Deploying
=========

`yarn build` creates a deployable production build.

Leap Motion Requirements
========================
Leap Motion Software is required for development or deployment. This application is compatible out of the box with the leap motion software 4.x, or Leapmotion 5.x-6.x (such as [LeapMotion Gemini](https://leap2.ultraleap.com/downloads/leap-motion-controller/))) using [this compatibility layer](https://github.com/ultraleap/UltraleapTrackingWebSocket).
