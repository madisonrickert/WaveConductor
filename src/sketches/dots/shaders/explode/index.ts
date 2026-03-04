import { ShaderPass } from "three-stdlib";
import { explodeShader } from "./shader";

export class ExplodeShaderPass extends ShaderPass {
    constructor() {
        super(explodeShader);
    }
}

export default ExplodeShaderPass;
