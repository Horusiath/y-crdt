import {transact, Doc} from "../doc.js";
import {AbstractType, errorObserveOnPrelimType, Event, TYPE_REFS_ARRAY} from "./abstract.js";
import {YArray as CoreArray} from "ywasm-core";

/**
 * A shared Array implementation.
 * @template T
 * @extends {AbstractType<CoreArray,Array<T>>}
 */
export class YArray extends AbstractType {
    /**
     *
     * @param {Doc} doc
     */
    constructor(doc) {
        super(doc, TYPE_REFS_ARRAY, [])
    }

    /**
     *
     * @return {number}
     */
    get length() {
        if (this.ytype !== null) {
            return transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                return inner.length(transaction)
            })
        } else {
            return this.prelim.length
        }
    }

    /**
     *
     * @param {number} index
     * @return {T}
     */
    get(index) {
        if (this.ytype !== null) {
            return transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                return inner.get(index, transaction)
            })
        } else {
            return this.prelim[index]
        }
    }

    /**
     * Inserts new content at an index.
     *
     * Important: This function expects an array of content. Not just a content
     * object. The reason for this "weirdness" is that inserting several elements
     * is very efficient when it is done as a single operation.
     *
     * @example
     *  // Insert character 'a' at position 0
     *  yarray.insert(0, ['a'])
     *  // Insert numbers 1, 2 at position 1
     *  yarray.insert(1, [1, 2])
     *
     * @param {number} index The index to insert content at.
     * @param {T[]} items
     */
    insert(index, items) {
        if (this.ytype !== null) {
            transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                inner.insert(index, items, transaction)
            })
        } else {
            this.prelim.splice(index, 0, ...items)
        }
    }

    /**
     * Appends content to this YArray.
     *
     * @param {T[]} items
     */
    push(items) {
        if (this.ytype !== null) {
            transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                inner.push(items, transaction)
            })
        } else {
            this.prelim.push(...items)
        }
    }

    /**
     * Deletes elements starting from an index.
     *
     * @param {number} index Index at which to start deleting elements
     * @param {number} length The number of elements to remove. Defaults to 1.
     */
    delete(index, length = 1) {
        if (this.ytype !== null) {
            transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                return inner.remove(index, length, transaction)
            })
        } else {
            return this.prelim.splice(index, length)
        }
    }

    /**
     *
     * @param {number} from
     * @param {number} to
     */
    move(from, to) {
        if (this.ytype !== null) {
            transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                return inner.move(from, to, transaction)
            })
        } else {
            const dest = to < from ? to : to - 1
            const moved = this.prelim.splice(from, 1)
            this.prelim.splice(dest, 0, ...moved)
        }
    }

    /**
     * Transforms this YArray to a JavaScript Array.
     *
     * @return {T[]}
     */
    toArray() {
        if (this.ytype !== null) {
            return transact(this.doc, (transaction) => {
                const inner = /** @type {CoreArray} */ (this.ytype)
                return inner.values(transaction)
            })
        } else {
            return [...this.prelim]
        }
    }

    /**
     * Transforms this Shared Type to a JSON object.
     *
     * @return {any}
     */
    toJson() {
        if (this.ytype !== null) {
            return transact(this.doc, (transaction) => {
                const inner = /** @type {YArray} */ (this.ytype)
                return inner.toJson(transaction)
            })
        } else {
            return this.prelim.map(c => c instanceof AbstractType ? c.toJson() : c)
        }
    }

    /**
     * Observes changes on the current array.
     * Returns a function which - when called - will unregister provided callback.
     *
     * @param {function(YArrayEvent): void} callback
     * @return {function}
     */
    observe(callback) {
        if (this.ytype) {
            const inner = /** @type {CoreArray} */ (this.ytype)
            let id = inner.observe(callback)
            return (() => {
                (/** @type {CoreArray} */ (this.ytype)).unobserve(id)
            })
        } else {
            throw errorObserveOnPrelimType
        }
    }

    /**
     * Observes changes on the current array and its nested (children) collections.
     * Returns a function which - when called - will unregister provided callback.
     *
     * @param {function(Event[]): void} callback
     * @return {function}
     */
    observeDeep(callback) {
        if (this.ytype) {
            const inner = /** @type {CoreArray} */ (this.ytype)
            let id = inner.observeDeep(callback)
            return (() => {
                (/** @type {CoreArray} */ (this.ytype)).unobserveDeep(id)
            })
        } else {
            throw errorObserveOnPrelimType
        }
    }
}

export class YArrayEvent extends Event {
    constructor() {
        super()
    }
}