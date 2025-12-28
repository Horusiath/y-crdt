import {exchangeUpdates} from './testHelper.js' // eslint-disable-line

import * as Y from 'ywasm'
import * as t from 'lib0/testing'
import {XmlElement} from "ywasm";

/**
 * @param {t.TestCase} tc
 */
export const testInsert = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')
    Y.transact(d1, txn => {
        root.push(new Y.XmlElement('p', {}, [
            new Y.XmlText('hello')
        ]))
        root.push(new Y.XmlText('world'))
    })

    const s = root.toString()

    t.compareStrings(s, '<p>hello</p>world')
}

/**
 * @param {t.TestCase} tc
 */
export const testAttributes = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')
    const xml = new Y.XmlElement('div', {}, [])
    root.push(xml)
    let actual = Y.transact(d1, () => {
        xml.setAttribute('key1', 'value1')
        xml.setAttribute('key2', 'value2')

        let obj = {}
        let attrs = xml.attributes();
        for (let key in attrs) {
            // we test iterator here
            obj[key] = attrs[key]
        }
        return obj
    });

    t.compareObjects(actual, {
        key1: 'value1',
        key2: 'value2'
    })

    actual = Y.transact(d1, () => {
        xml.removeAttribute('key1')
        return {
            key1: xml.getAttribute('key1'),
            key2: xml.getAttribute('key2')
        }
    })

    t.compareObjects(actual, {
        key1: undefined,
        key2: 'value2'
    })
}

export const testAttributesAny = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')
    const xml = new Y.XmlElement('div', {}, [])
    root.push(xml)
    let actual = Y.transact(d1, txn => {
        xml.setAttribute('key1', true)
        xml.setAttribute('key2', 42)
        xml.setAttribute('key3', null)

        let obj = {}
        let attrs = xml.attributes();
        for (let key in attrs) {
            // we test iterator here
            obj[key] = attrs[key]
        }
        return obj
    });

    t.compareObjects(actual, {
        key1: true,
        key2: 42,
        key3: null
    })

    actual = Y.transact(d1, () => {
        xml.removeAttribute('key1')
        return {
            key1: xml.getAttribute('key1'),
            key2: xml.getAttribute('key2'),
            key3: xml.getAttribute('key3')
        }
    })

    t.compareObjects(actual, {
        key1: undefined,
        key2: 42,
        key3: null
    })
}

export const testAttributesPrelim = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')

    let xml
    let actual = Y.transact(d1, () => {
        xml = new Y.XmlElement('div', {}, [])
        xml.setAttribute('key1', true)
        xml.setAttribute('key2', 42)
        xml.setAttribute('key3', null)

        root.push(xml)

        let obj = {}
        let attrs = xml.attributes();
        for (let key in attrs) {
            // we test iterator here
            obj[key] = attrs[key]
        }
        return obj
    });

    t.compareObjects(actual, {
        key1: true,
        key2: 42,
        key3: null
    })

    actual = Y.transact(d1, () => {
        xml.removeAttribute('key1')
        return {
            key1: xml.getAttribute('key1'),
            key2: xml.getAttribute('key2'),
            key3: xml.getAttribute('key3')
        }
    })

    t.compareObjects(actual, {
        key1: undefined,
        key2: 42,
        key3: null
    })
}

export const testAttributesCtor = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')
    const xml = new Y.XmlElement('div', {"key1": false}, [])
    root.push(xml)

    let attrs = xml.attributes();
    let obj = {}
    for (let key in attrs) {
        // we test iterator here
        obj[key] = attrs[key]
    }

    t.compareObjects(attrs, {
        key1: false,
    })
}

/**
 * @param {t.TestCase} tc
 */
export const testSiblings = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')
    const first = Y.transact(d1, () => {
        const a = new Y.XmlElement('p', {}, [
            new Y.XmlText('hello')
        ])
        root.push(a)
        root.push(new Y.XmlText('world'))

        return a
    })

    t.compare(first.prevSibling(), undefined)

    let second = first.nextSibling()
    let s = second.toString()
    t.compare(s, 'world')
    t.compare(second.nextSibling(), undefined)

    let actual = second.prevSibling().toString()
    let expected = first.toString()
    t.compare(actual, expected)
}

/**
 * @param {t.TestCase} tc
 */
export const testTreeWalker = tc => {
    const d1 = new Y.Doc()
    const root = d1.getXmlFragment('test')
    Y.transact(d1, () => {
        root.push(new Y.XmlElement('p', {}, [
            new Y.XmlText('hello')
        ]))
        root.push(new Y.XmlText('world'))
    })

    const actual = []
    Y.transact(d1, () => {
        for (let child of root.treeWalker()) {
            let str = child.toString()
            actual.push(str)
        }
    })

    const expected = [
        '<p>hello</p>',
        'hello',
        'world'
    ]
    t.compareArrays(actual, expected)
}

/**
 * @param {t.TestCase} tc
 */
export const testXmlTextObserver = tc => {
    const d1 = new Y.Doc()
    const f = d1.getXmlFragment('test');
    const x = new Y.XmlText()
    f.push(x)
    let target = null
    let attributes = null
    let delta = null
    let origin = null
    let callback = e => {
        target = e.target
        attributes = e.keys
        delta = e.delta
        origin = e.origin
    }
    x.observe(callback)

    // set initial attributes
    Y.transact(d1, () => {
        x.setAttribute('attr1', 'value1')
        x.setAttribute('attr2', 'value2')
    }, 'TEST_ORIGIN')
    t.compare(target.toString(), x.toString())
    t.compare(delta, [])
    t.compare(attributes, {
        attr1: {action: 'add', newValue: 'value1'},
        attr2: {action: 'add', newValue: 'value2'}
    })
    t.compare(origin, 'TEST_ORIGIN')
    target = null
    attributes = null
    delta = null

    // update attributes
    Y.transact(d1, () => {
        x.setAttribute('attr1', 'value11')
        x.removeAttribute('attr2')
    }, 'TEST_ORIGIN2')
    t.compare(target.toString(), x.toString())
    t.compare(delta, [])
    t.compare(attributes, {
        attr1: {action: 'update', oldValue: 'value1', newValue: 'value11'},
        attr2: {action: 'delete', oldValue: 'value2'}
    })
    t.compare(origin, 'TEST_ORIGIN2')
    target = null
    attributes = null
    delta = null

    // insert initial data to an empty YText
    x.insert(0, 'abcd')
    t.compare(target.toString(), x.toString())
    t.compare(delta, [{insert: 'abcd'}])
    t.compare(attributes, {})
    target = null
    attributes = null
    delta = null

    // remove 2 chars from the middle
    x.delete(1, 2)
    t.compare(target.toString(), x.toString())
    t.compare(delta, [{retain: 1}, {delete: 2}])
    t.compare(attributes, {})
    target = null
    attributes = null
    delta = null

    // insert new item in the middle
    x.insert(1, 'e')
    t.compare(target.toString(), x.toString())
    t.compare(delta, [{retain: 1}, {insert: 'e'}])
    t.compare(attributes, {})
    target = null
    attributes = null
    delta = null

    // free the observer and make sure that callback is no longer called
    t.assert(x.unobserve(callback), 'unobserve failed')
    x.insert(1, 'fgh')
    t.compare(target, null)
    t.compare(attributes, null)
    t.compare(delta, null)
}
/**
 * @param {t.TestCase} tc
 */
export const testXmlElementObserver = tc => {
    const d1 = new Y.Doc()
    const f = d1.getXmlFragment('test');
    const x = new Y.XmlElement('div')
    f.push(x)
    let target = null
    let attributes = null
    let nodes = null
    let callback = e => {
        target = e.target
        attributes = e.keys
        nodes = e.delta
    }
    x.observe(callback)

    // insert initial attributes
    Y.transact(d1, () => {
        x.setAttribute('attr1', 'value1')
        x.setAttribute('attr2', 'value2')
    })
    t.compare(target.toString(), x.toString())
    t.compare(nodes, [])
    t.compare(attributes, {
        attr1: {action: 'add', newValue: 'value1'},
        attr2: {action: 'add', newValue: 'value2'}
    })
    target = null
    attributes = null
    nodes = null

    // update attributes
    Y.transact(d1, () => {
        x.setAttribute('attr1', 'value11')
        x.removeAttribute('attr2')
    })
    t.compare(target.toString(), x.toString())
    t.compare(nodes, [])
    t.compare(attributes, {
        attr1: {action: 'update', oldValue: 'value1', newValue: 'value11'},
        attr2: {action: 'delete', oldValue: 'value2'}
    })
    target = null
    attributes = null
    nodes = null

    // add children
    Y.transact(d1, txn => {
        x.push(new Y.XmlElement('div'))
        x.push(new Y.XmlElement('p'))
    })
    t.compare(target.toString(), x.toString())
    t.compare(nodes[0].insert.length, 2) // [{ insert: [div, p] }]
    t.compare(attributes, {})
    target = null
    attributes = null
    nodes = null

    // remove a child
    x.delete(0, 1)
    t.compare(target.toString(), x.toString())
    t.compare(nodes, [{delete: 1}])
    t.compare(attributes, {})
    target = null
    attributes = null
    nodes = null

    // insert child again
    let txt = new Y.XmlText()
    x.push(txt)
    t.compare(target.toString(), x.toString())
    t.compare(nodes[0], {retain: 1});
    t.assert(nodes[1].insert != null)
    t.compare(attributes, {})
    target = null
    attributes = null
    nodes = null

    // free the observer and make sure that callback is no longer called
    t.assert(x.unobserve(callback), 'unobserve failed')
    x.insert(0, new Y.XmlElement('head'))
    t.compare(target, null)
    t.compare(nodes, null)
    t.compare(attributes, null)
}