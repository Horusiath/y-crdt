import {AbstractType, TYPE_REFS_MAP} from "./abstract.js"
import {YMap as CoreMap} from 'ywasm-core'

/**
 *
 * @template T
 * @extends {AbstractType<CoreMap,Map<string,T>>}
 */
export class YMap extends AbstractType {
    /**
     *
     * @param {Doc} doc
     */
    constructor(doc) {
        super(doc, TYPE_REFS_MAP, new Map())
    }
}