import {AbstractType, TYPE_REFS_TEXT} from "./abstract.js";


export class YText extends AbstractType {
    /**
     *
     * @param {Doc} doc
     */
    constructor(doc) {
        super(doc, TYPE_REFS_TEXT, '')
    }
}