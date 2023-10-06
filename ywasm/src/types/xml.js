import {AbstractType, TYPE_REFS_XML_ELEMENT, TYPE_REFS_XML_FRAGMENT, TYPE_REFS_XML_TEXT} from "./abstract.js";
import {YText} from "./text.js";


export class YXmlText extends YText {
    /**
     *
     * @param {Doc} doc
     */
    constructor(doc) {
        super(doc)
        this.__kind = TYPE_REFS_XML_TEXT
    }
}

export class YXmlFragment extends AbstractType {
    /**
     *
     * @param {Doc} doc
     */
    constructor(doc) {
        super(doc, TYPE_REFS_XML_FRAGMENT, [])
    }
}

export class YXmlElement extends YXmlFragment {
    /**
     *
     * @param {Doc} doc
     */
    constructor(doc) {
        super(doc)
        this.__kind = TYPE_REFS_XML_ELEMENT
    }
}