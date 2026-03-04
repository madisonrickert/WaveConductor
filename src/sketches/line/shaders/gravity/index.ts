import { ShaderPass } from "three-stdlib";
import { gravityShader } from "./shader";

export class GravityShaderPass extends ShaderPass {
    constructor() {
        super(gravityShader);
    }
}

export default GravityShaderPass;
