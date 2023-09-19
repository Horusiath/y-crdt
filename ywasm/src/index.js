import * as core from 'ywasm-core'

export class Doc {
    constructor({clientID = null, skipGc = false}) {
        this.ydoc = new core.YDoc()
        /** @type {Transaction|null} */
        this.transaction = null
    }
}

export class Transaction {
    constructor() {
    }
}

export class AbstractType {
    constructor() {
        /** @type {any} */
        this.ytype = null
        /** @type {Doc} */
        this.doc = null
    }
}

export class Array extends AbstractType {
    constructor() {
        super()
        /** @type {any[]} */
        this.prelim = []
    }

    get(index) {
        if (this.doc !== null) {
            return transact(this.doc, (transaction) => {
                return this.ytype.get(index, transaction)
            })
        } else {
            return this.prelim[index]
        }
    }

    insert(index) {

    }
}

export class Map extends AbstractType {
    constructor() {
        super()
        this.prelim = new Map()
    }
}

export class Text extends AbstractType {
    constructor() {
        super()
        this.prelim = ''
    }
}

export class XmlText extends Text {
    constructor() {
        super()
    }
}

export class XmlFragment extends AbstractType {
    constructor() {
        super()
        /** @type {(XmlFragment|XmlText)[]} */
        this.prelim = []
    }
}

export class XmlElement extends XmlFragment {
    constructor() {
        super()
    }
}

/**
 * 
 * @param {Doc} doc 
 * @param {(Transaction) => any} f 
 */
export const transact = (doc, f) => {
    /** @type {Transaction} */
    const transaction = doc.transaction || doc.ydoc.startTransaction()
    doc.transaction = transaction
    try {
        return f(transaction)
    } finally {
        transaction.free()
        doc.transaction = null
    }
}