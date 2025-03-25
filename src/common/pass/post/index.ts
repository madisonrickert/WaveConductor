import { ShaderPass } from "three-stdlib";
import { PostShader } from "./shader";

export class PostPass extends ShaderPass {
    constructor() {
        super(PostShader);
    }
}

export default PostPass;
