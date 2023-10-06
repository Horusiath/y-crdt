import * as Y from '../src/index.js'

/**
 * @param {Array<Y.Doc>} docs
 */
export const exchangeUpdates = docs => {
    for(let d1 of docs) {
        for(let d2 of docs) {
            if (d1 !== d2) {
                let stateVector = Y.encodeStateVector(d1)
                let diff = Y.encodeStateAsUpdate(d2, stateVector)

                Y.applyUpdate(d1, diff, "exchange")
            }
        }
    }
}