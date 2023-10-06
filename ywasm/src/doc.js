import * as core from 'ywasm-core'
import {YArray} from "./types/array.js";
import {YMap} from "./types/map.js";
import {YText} from "./types/text.js";
import {YXmlFragment} from "./types/xml.js";

/**
 * @typedef {()=>void} Subscription callback that unsubscribes registered observer.
 */

/**
 *
 */
export class Doc {
    /**
     *
     * @param {{clientID?:number,guid?:string,collectionID?:string,gc?:boolean,autoLoad?:boolean,shouldLoad?:boolean}|YDoc} options
     */
    constructor(options= null) {
        this.ydoc = options instanceof core.YDoc ? options : new core.YDoc(options)
        /** @type {Transaction|null} */
        this.transaction = null
        /** @type {Doc|null} */
        this._parent = null
    }

    /**
     *
     * @return {Doc|null}
     */
    get parent() {
        if (this._parent === null)  {
            this._parent = new Doc(this.ydoc.parentDoc)
        }
        return this._parent
    }

    /**
     *
     * @return {number}
     */
    get id() {
        return this.ydoc.id
    }

    /**
     *
     * @return {string}
     */
    get guid() {
        return this.ydoc.guid
    }

    /**
     *
     * @return {boolean}
     */
    get autoLoad() {
        return this.ydoc.autoLoad
    }

    /**
     *
     * @return {boolean}
     */
    get shouldLoad() {
        return this.ydoc.shouldLoad
    }

    transact(callback, origin = null) {
        return transact(this, callback, origin)
    }

    /**
     *
     * @param {string} name
     * @template T
     * @returns {YArray<T>}
     */
    getArray(name) {
        const result = new YArray(this)
        result.ytype = this.ydoc.getArray(name)
        return result
    }

    /**
     *
     * @param {string} name
     * @returns {YMap<T>}
     */
    getMap(name) {
        const result = new YMap(this)
        result.ytype = this.ydoc.getMap(name)
        return result
    }

    /**
     *
     * @param {string} name
     * @returns {YText}
     */
    getText(name) {
        const result = new YText(this)
        result.ytype = this.ydoc.getText(name)
        return result
    }

    /**
     *
     * @param {string} name
     * @returns {YXmlFragment}
     */
    getXmlFragment(name) {
        const result = new YXmlFragment(this)
        result.ytype = this.ydoc.getXmlFragment(name)
        return result
    }

    load() {
        transact(this.parent, transaction => {
            this.ydoc.load(transaction)
        })
    }

    destroy() {
        transact(this.parent, transaction => {
            this.ydoc.destroy(transaction)
        })
    }

    /**
     *
     * @returns {Doc[]}
     */
    getSubdocs() {
        return transact(this, transaction => {
            return this.ydoc.getSubdocs(transaction)
        })
    }

    /**
     *
     * @return {Set<string>}
     */
    getSubdocsGuids() {
        return transact(this, transaction => {
            return this.ydoc.getSubdocGuids(transaction)
        })
    }

    /**
     * Registers a function called whenever a document contents are being changed.
     * Returns a function, which - when called - will unregister callback method.
     *
     * @param {function(Uint8Array,any): void} callback Function called when document has been updated.
     *          First argument is a lib0 v1 encoded update containing all the changes.
     *          Second argument is optional transaction origin.
     * @returns {Subscription} Function, which upon calling, will unregister this callback.
     */
    onUpdate(callback) {
        const id = this.ydoc.observeUpdate(callback)
        return (() => {
            this.ydoc.unobserveUpdate(id)
        })
    }

    /**
     * Registers a function called whenever a document contents are being changed.
     * Returns a function, which - when called - will unregister callback method.
     *
     * @param {function(Uint8Array,any): void} callback Function called when document has been updated.
     *          First argument is a lib0 v2 encoded update containing all the changes.
     *          Second argument is optional transaction origin.
     * @returns {Subscription} Function, which upon calling, will unregister this callback.
     */
    onUpdateV2(callback) {
        const id = this.ydoc.observeUpdateV2(callback)
        return (() => {
            this.ydoc.unobserveUpdateV2(id)
        })
    }

    /**
     *
     * @param callback
     * @returns {Subscription} Function, which upon calling, will unregister this callback.
     */
    onSubdocs(callback) {
        const id = this.ydoc.observeSubdocs(callback)
        return (() => {
            this.ydoc.unobserveSubdocs(id)
        })
    }

    /**
     *
     * @param callback
     * @returns {Subscription} Function, which upon calling, will unregister this callback.
     */
    onDestroy(callback) {
        const id = this.ydoc.observeDestroy(callback)
        return (() => {
            this.ydoc.unobserveDestroy(id)
        })
    }

    /**
     *
     * @param callback
     * @returns {Subscription} Function, which upon calling, will unregister this callback.
     */
    onAfterTransaction(callback) {
        const id = this.ydoc.observeAfterTransaction(callback)
        return (() => {
            this.ydoc.unobserveAfterTransaction(id)
        })
    }
}

/**
 *
 * @template T
 * @param {Doc} doc
 * @param {function(Transaction): T} f
 * @param {any} origin
 * @returns {T}
 */
export const transact = (doc, f, origin = null) => {
    /** @type {Transaction} */
    const transaction = doc.transaction || doc.ydoc.startTransaction(origin)
    doc.transaction = transaction
    try {
        return f(transaction)
    } finally {
        transaction.free()
        doc.transaction = null
    }
}

/**
 * Apply a document update created by, for example, `y.on('update', update => ..)` or `update = encodeStateAsUpdate()`.
 *
 * @param {Doc} doc
 * @param {Uint8Array} update
 * @param {any} [origin] This will be stored on `transaction.origin` and `.on('update', (update, origin))`
 *
 * @function
 */
export const applyUpdate = (doc, update, origin) => {
    transact(doc, transaction => {
        transaction.applyUpdate(update)
    }, origin)
}

/**
 * Apply a document update (encoded using lib0 v2 encoding) created by, for example,
 * `y.on('update', update => ..)` or `update = encodeStateAsUpdate()`.
 *
 * @param {Doc} doc
 * @param {Uint8Array} update
 * @param {any} [origin] This will be stored on `transaction.origin` and `.on('update', (update, origin))`
 *
 * @function
 */
export const applyUpdateV2 = (doc, update, origin) => {
    transact(doc, transaction => {
        transaction.applyUpdateV2(update)
    }, origin)
}

/**
 * Write all the document as a single update message that can be applied on the remote document. If you specify the state of the remote client (`targetState`) it will
 * only write the operations that are missing.
 *
 * Use `writeStateAsUpdate` instead if you are working with lib0/encoding.js#Encoder
 *
 * @param {Doc} doc
 * @param {Uint8Array} [encodedTargetStateVector] The state of the target that receives the update. Leave empty to write all known structs
 * @return {Uint8Array}
 *
 * @function
 */
export const encodeStateAsUpdate = (doc, encodedTargetStateVector = new Uint8Array([0])) => {
    return transact(doc, transaction => {
        return transaction.encodeDocStateAsUpdate(encodedTargetStateVector)
    })
}

/**
 * Write all the document as a single update message that can be applied on the remote document. If you specify the state of the remote client (`targetState`) it will
 * only write the operations that are missing.
 *
 * Use `writeStateAsUpdate` instead if you are working with lib0/encoding.js#Encoder
 *
 * @param {Doc} doc
 * @param {Uint8Array} [encodedTargetStateVector] The state of the target that receives the update. Leave empty to write all known structs
 * @return {Uint8Array}
 *
 * @function
 */
export const encodeStateAsUpdateV2 = (doc, encodedTargetStateVector = new Uint8Array([0])) => {
    return transact(doc, transaction => {
        return transaction.encodeDocStateAsUpdateV2(encodedTargetStateVector)
    })
}

/**
 * Encode State as Uint8Array.
 *
 * @param {Doc} doc
 * @return {Uint8Array}
 *
 * @function
 */
export const encodeStateVector = (doc) => {
    return transact(doc, transaction => {
        return transaction.encodeStateVector()
    })
}