

export const TYPE_REFS_ARRAY = 0
export const TYPE_REFS_MAP = 1
export const TYPE_REFS_TEXT = 2
export const TYPE_REFS_XML_ELEMENT = 3
export const TYPE_REFS_XML_FRAGMENT = 4
export const TYPE_REFS_XML_HOOK = 5
export const TYPE_REFS_XML_TEXT = 6
export const TYPE_REFS_DOC = 9

/**
 * Abstract class shared between all y-type collections.
 * @template {T} T template for underlying ywasm-core type
 * @template {P} P template for preliminary type
 */
export class AbstractType {
    /**
     *
     * @param {Doc} doc
     * @param {number} kind
     * @param {P} prelim
     */
    constructor(doc, kind, prelim) {
        if (!doc) {
            throw new Error('cannot instantiate shared type without specified Doc')
        }
        this.__kind = kind
        /** @type {Doc} */
        this.doc = doc
        /** @type {P} */
        this.prelim = prelim
        /** @type {T} */
        this.ytype = null
    }

    toJson() {
        throw new Error('not implemented')
    }
}

export const errorObserveOnPrelimType = Error('cannot call observe on shared type not integrated into document')

export class Event {

}