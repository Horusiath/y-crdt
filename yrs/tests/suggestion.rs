use yrs::lib0::{Decode, Encode, Encoder, EncoderV1};
use yrs::node::Attrs;
use yrs::test_utils::{clone_doc, exchange_updates};
use yrs::{
    Acquire, AcquireMut, Any, Attribute, Attribution, Cell, ClientID, ContentIds, ContentMap,
    Delta, DeltaOptions, DiffFormatter, Doc, ID, IdSet, In, Options, StateVector, Suggestion,
    Update, any,
};

fn docs(base: u64, sugg: u64) -> (Cell<Doc>, Cell<Doc>) {
    let prev = Doc::with_options(Options {
        skip_gc: true,
        ..Options::with_client_id(ClientID::new(base))
    });
    let next = Doc::with_options(Options {
        skip_gc: true,
        cleanup_formatting: false, // suggestion doc
        ..Options::with_client_id(ClientID::new(sugg))
    });
    (Cell::new(prev), Cell::new(next))
}

fn session(prev: &Cell<Doc>, next: &Cell<Doc>) -> Suggestion {
    Suggestion::new(prev.clone(), next.clone(), Some(ContentMap::default()))
}

fn apply(doc: &Cell<Doc>, node: &str, d: Delta<In>) {
    let mut doc = doc.acquire_mut();
    doc.transact_mut().node_mut(node).unwrap().apply_delta(d);
}

/// Applies a delta `d` to a root `node` of a suggestion `doc`, as it renders. Returns the fix.
fn suggest(s: &Suggestion, doc: &Cell<Doc>, node: &str, d: Delta<In>) -> Delta<In> {
    let mut doc = doc.acquire_mut();
    let mut txn = doc.transact_mut();
    let fmt = s.formatter();
    txn.node_mut(node)
        .unwrap()
        .formatted_mut(&*fmt)
        .apply_delta(d)
}

/// A fresh deep render of a root `node` of a suggestion `doc`.
fn render(s: &Suggestion, doc: &Cell<Doc>, node: &str) -> Delta<In> {
    let doc = doc.acquire();
    let txn = doc.transact();
    let node = txn.node(node).unwrap();
    let fmt = s.formatter();
    node.formatted(&*fmt).delta(&DeltaOptions::deep()).into()
}

/// The content of a root `node` of a `doc` (Yjs `toDeltaDeep`), empty if it doesn't exist.
fn content(doc: &Cell<Doc>, node: &str) -> Delta<In> {
    let doc = doc.acquire();
    let txn = doc.transact();
    let node = txn.node(node);
    node.map_or_else(Delta::new, |node| node.delta(&DeltaOptions::deep()).into())
}

fn el(name: &str, d: Delta<In>) -> In {
    In::from(Delta {
        name: Some(name.into()),
        ..d
    })
}

fn paragraph(text: &str) -> In {
    el("paragraph", Delta::new().insert_text(text))
}

/// `<blockquote><paragraph>deep</paragraph></blockquote>`
fn quote() -> In {
    el("blockquote", Delta::new().insert(paragraph("deep")))
}

/// `<image src="x.png"/>`
fn image() -> In {
    el("image", Delta::new().insert_attr("src", "x.png"))
}

fn inserted() -> Option<Attribution> {
    Some(Attribution::insert::<Attribute>([]))
}

/// Formatting of `keys` attributed to nobody in particular.
fn formatted(keys: &[&str]) -> Option<Attribution> {
    let format = keys.iter().map(|&key| (key.into(), Some(vec![])));
    Some(Attribution {
        format: Some(format.collect()),
        ..Default::default()
    })
}

fn fmt(key: &str, value: Any) -> Attrs {
    Attrs::from([(key.into(), value)])
}

/// Yjs `testRdtAcceptingNodeInsertRenderedAsSuggestion`:
/// Regression pin (originally a failing repro) — the consumer-visible framing of the cache drift above: inserting a
/// node through an *accepting* renderer (`suggestionMode = false`) must not leave it presented as
/// a suggestion. The write currently emits two `'delta'` events: first the insert render, fully
/// attributed (`{insert: []}` on the inserted node AND its nested content), then a de-attribution
/// correction `retain(1).retain(1, {attribution: null})` once the content commits to base — but
/// the correction only clears the attribution on the node itself and never descends into the
/// node's children. Composing the event stream (which is exactly how the maintained `.delta`
/// cache is built) therefore leaves the nested text attributed as a suggested insert forever,
/// while ground truth (a fresh deep render) shows committed, unattributed content. The test does
/// not prescribe the fix: it passes if the insert render arrives unattributed OR if the
/// correction descends — it only requires the settled event stream to converge to the truth.
#[test]
fn accepting_node_insert_rendered_as_suggestion() {
    let (prev, next) = docs(1, 2);
    let s = session(&prev, &next);
    s.set_suggestion_mode(false);
    apply(
        &prev,
        "prosemirror",
        Delta::new().insert(paragraph("base para")),
    ); // <prosemirror><p>base para</p></prosemirror>
    let d = Delta::new().retain(1).insert(paragraph("plain"));
    suggest(&s, &next, "prosemirror", d); // <prosemirror><p>base para</p><p>plain</p></prosemirror>
    // the insert committed to base, nothing is suggested
    let expected = Delta::new()
        .insert(paragraph("base para"))
        .insert(paragraph("plain"));
    assert_eq!(render(&s, &next, "prosemirror"), expected);
    // yrs has no `'delta'` channel to compose:
    // t.assert(!JSON.stringify(composed.toJSON()).includes('"attribution"'))
    // t.assert(composed.equals(fresh))
    suggest(
        &s,
        &next,
        "prosemirror",
        Delta::new().retain(2).insert(quote()),
    );
    let expected = Delta::new()
        .insert(paragraph("base para"))
        .insert(paragraph("plain"))
        .insert(quote());
    assert_eq!(render(&s, &next, "prosemirror"), expected);
    assert_eq!(content(&prev, "prosemirror"), expected);
}

/// Yjs `testRdtPartialAcceptKeepsPendingChildSuggestions`:
/// Guards the id-scoped heal design: unattributing an accepted node must never cascade into a
/// blanket subtree clear, because children may still be attributed. Here only the node's OWN
/// insert struct is committed to base — `acceptChanges` ships an accepted node together with its
/// subtree (see {@link testRdtRangeAcceptShipsNestedStructs}), so the partial state is built by
/// hand through the raw struct path the accept uses — the node commits to base as an empty
/// paragraph while its text children remain pending suggestions. The heal must clear the node's
/// attribution, keep the children's `{insert: []}`, and keep the maintained cache equal to a fresh
/// render.
#[test]
fn partial_accept_keeps_pending_child_suggestions() {
    let (prev, next) = docs(1, 2);
    let s = session(&prev, &next);
    apply(
        &prev,
        "prosemirror",
        Delta::new().insert(paragraph("base para")),
    );
    let d = Delta::new().retain(1).insert(paragraph("x")); // node at clock 0, 'x' at 1
    suggest(&s, &next, "prosemirror", d);
    let d = Delta::new()
        .retain(1)
        .modify(Delta::new().retain(1).insert_text("Q"));
    suggest(&s, &next, "prosemirror", d);
    let xq = || {
        el(
            "paragraph",
            Delta::new().insert_text_attributed("xQ", None, inserted()),
        )
    };
    let expected = Delta::new()
        .insert(paragraph("base para"))
        .insert_range_attributed([xq()], None, inserted());
    assert_eq!(render(&s, &next, "prosemirror"), expected);
    // commit ONLY the node's own struct, every child stays a pending suggestion
    let mut encoder = EncoderV1::new();
    let node_only = IdSet::from_iter([(ClientID::new(2), [0..1])]);
    next.acquire().write_selected(&node_only, &mut encoder);
    IdSet::new().encode(&mut encoder);
    let update = Update::decode_v1(&encoder.to_vec()).unwrap();
    prev.acquire_mut()
        .transact_mut()
        .apply_update(update)
        .unwrap();
    let empty = el("paragraph", Delta::new());
    let base = Delta::new().insert(paragraph("base para")).insert(empty);
    assert_eq!(content(&prev, "prosemirror"), base);
    let expected = Delta::new().insert(paragraph("base para")).insert(xq());
    assert_eq!(render(&s, &next, "prosemirror"), expected);
}

/// Suggests a node insert (its struct at clock 0) into a base `<paragraph>ab</paragraph>`, then
/// accepts it.
fn accept_nested(d: Delta<In>) -> (Delta<In>, Delta<In>) {
    let (prev, next) = docs(0, 1);
    let s = session(&prev, &next);
    apply(&prev, "prosemirror", Delta::new().insert(paragraph("ab")));
    suggest(&s, &next, "prosemirror", d);
    s.accept_changes(
        ID::new(ClientID::new(1), 0),
        Some(ID::new(ClientID::new(1), 0)),
    );
    (
        content(&prev, "prosemirror"),
        render(&s, &next, "prosemirror"),
    )
}

/// Yjs `testRdtRangeAcceptShipsNestedStructs` (this and the two tests below are its `check` cases):
/// A range accept ships an accepted node together with its subtree. `collectSuggestedChanges` used
/// to collect only the top-level item chain of the accepted range, so accepting a pending node
/// insert committed the bare node struct: an image reached the base doc without its `src`, a
/// paragraph without its text, a blockquote without its paragraphs (found through y-prosemirror's
/// cohort fuzz — its known issue 7 — where a base-bound editor then threw on the image without its
/// required attr). A nested item can only exist in the suggestion doc if its parent does, so the
/// subtree is part of the same suggestion.
#[test]
fn range_accept_ships_node_attrs() {
    let d = Delta::new().modify(Delta::new().retain(1).insert(image()));
    let (base, fresh) = accept_nested(d);
    let expected = Delta::new().insert(el(
        "paragraph",
        Delta::new()
            .insert_text("a")
            .insert(image())
            .insert_text("b"),
    ));
    assert_eq!(base, expected);
    assert_eq!(fresh, expected);
}

#[test]
fn range_accept_ships_node_children() {
    let d = Delta::new().retain(1).insert(paragraph("text"));
    let (base, fresh) = accept_nested(d);
    let expected = Delta::new()
        .insert(paragraph("ab"))
        .insert(paragraph("text"));
    assert_eq!(base, expected);
    assert_eq!(fresh, expected);
}

#[test]
fn range_accept_ships_nested_nodes() {
    let (base, fresh) = accept_nested(Delta::new().retain(1).insert(quote()));
    let expected = Delta::new().insert(paragraph("ab")).insert(quote());
    assert_eq!(base, expected);
    assert_eq!(fresh, expected);
}

/// Yjs `testRdtAcceptAllOfSameValueSuggestedFormatCacheDrift`:
/// Regression pin — fixed by provenance tracking (`currentFormatsAttributed`) widening
/// `isDeletedFormatClear` to committed enclosing values: the accept deletes the committed run's
/// close marker, whose removal re-exposes the committed `strong={}`, and the old
/// `currFormatVal == null` guard blocked the needed clear. (Lifecycle-fuzz class P2, 23/3000
/// seeds; minimized from seed 259's 9 ops to 3): ACCEPT-triggered cache drift — every step
/// before the accept is consistent, and `acceptAllChanges()` itself corrupted the maintained
/// `.delta`. Shape: a committed `strong` run
/// ('a', present since the very first render) with a plain char ('b') right after it; a suggested
/// `strong` on 'b' with the SAME key AND the SAME value as the committed run; accept-all. The
/// accept commits the suggestion to base, and a fresh deep render shows one fully-committed
/// unattributed run — but the accept's change render never clears the suggestion's format
/// attribution on 'b', leaving the stale `{format:{strong:[]}}` in the cache forever:
///
///   cached: a(strong) | b(strong, {format:{strong:[]}} ← STALE)
///   fresh : ab(strong)
///
/// Everything is load-bearing: same key (strong/em → clean), same value ({} vs {x:1} or null →
/// clean), adjacency (a gap between the runs → clean), and order (committed run must precede the
/// suggested char; suggesting on the char *before* the run, or on the run itself, renders clean).
/// Both clientID orders reproduce. Related but distinct from
/// testRdtAcceptingNodeInsertCacheDrift (that class leaks `{insert:[]}` into nested node content;
/// this one leaks a `format` attribution on plain text, with a clean pre-accept state).
#[test]
fn accept_all_of_same_value_suggested_format() {
    let (prev, next) = docs(0, 1);
    let s = session(&prev, &next);
    let strong = fmt("strong", any!({}));
    apply(
        &prev,
        "t",
        Delta::new().insert_text_fmt("a", &strong).insert_text("b"),
    );
    suggest(
        &s,
        &next,
        "t",
        Delta::new().retain(1).retain_fmt(1, &strong),
    );
    let expected = Delta::new()
        .insert_text_fmt("a", &strong)
        .insert_text_attributed("b", Some(&strong), formatted(&["strong"]));
    assert_eq!(render(&s, &next, "t"), expected);
    s.accept_all_changes();
    // t.assert(cached.equals(fresh))
    let expected = Delta::new().insert_text_fmt("ab", &strong);
    assert_eq!(render(&s, &next, "t"), expected);
    assert_eq!(content(&prev, "t"), expected);
}

/// Yjs `testRdtRejectAllOverlappingSameKeyFormatProvenanceResidual`:
/// Regression pin — the long-documented reject-side provenance residual, fixed by the same
/// `currentFormatsAttributed` provenance tracking as the accept class above (the failing render
/// is the sugg-doc EVENT render — the reject's heal render receives an empty change set — and
/// the deleted suggestion marker re-exposes the enclosing *committed* base strong, which the old
/// `currFormatVal == null` guard could not see). Lifecycle-fuzzing class P1, 45 seeds; minimized
/// from the 9-op seed-132 representative to 4 ops: a suggested strong on 'a' under a base strong
/// over all of 'ab' — same key, with the base range extending strictly *past* the suggested
/// span — then rejectAllChanges. The reject clears the suggestion, but the change render left
/// the trailing 'b' still carrying the suggested-format attribution in the cache; the fresh
/// render shows it unattributed:
///
///   cached: a(strong) | b(strong, {format:{strong:[]}} ← STALE)
///   fresh : ab(strong)
///
/// Everything here is load-bearing: without the suggested format, without the base format, with
/// a different suggested key (em), with the base range ending where the suggested span ends
/// (full overlap), or with the sugg span at the end of the base range, it renders clean. The
/// pre-reject cache is consistent — the residual appears only at the reject. The marker
/// integration order is load-bearing in the opposite direction of the two-key siblings above:
/// it reproduces with base.clientID < sugg.clientID and renders clean flipped. The reject flow
/// does not echo structs back here (both clientIDs survive unreassigned), so the repro is
/// Math.random-free: 100/100 identical failing runs without seeding.
#[test]
fn reject_all_overlapping_same_key_format() {
    let (prev, next) = docs(0, 1);
    let s = session(&prev, &next);
    let strong = fmt("strong", any!({}));
    apply(&prev, "t", Delta::new().insert_text("ab"));
    suggest(&s, &next, "t", Delta::new().retain_fmt(1, &strong));
    apply(&prev, "t", Delta::new().retain_fmt(2, &strong));
    let expected = Delta::new()
        .insert_text_fmt("a", &strong)
        .insert_text_attributed("b", None, formatted(&["strong"]));
    assert_eq!(render(&s, &next, "t"), expected);
    s.reject_all_changes();
    // t.assert(cached.equals(fresh))
    // atm. yrs has no delta cache so we only assert the correct delta generation
    let expected = Delta::new().insert_text_fmt("ab", &strong);
    assert_eq!(render(&s, &next, "t"), expected);
    assert_eq!(content(&prev, "t"), expected);
}

/// Yjs `testRdtSplitFormatMarkerPairPartialAcceptThenUnformatCacheDrift`:
/// Regression pin — fixed by re-adding the reset key as an explicit clear when the ambient-copy
/// install is forced by the all-null-leaves disjunct (installing after silently dropping the key
/// would close its governance without the clear the fresh render's context-close implies):
/// partial-accept cache drift when the accepted id range SPLITS a suggested format's
/// open/close ContentFormat marker pair. Lifecycle-fuzz seed 648 (failIndex 14, 15 ops incl. an
/// accept and a reject) minimized to 5 ops with no inserts/deletes at all.
///
/// Shape: two different-key suggested formats on the same char 'A' of base text 'AB'. The sugg
/// client's structs are exactly four ContentFormat markers — clock 0 = em open ({em:{}}),
/// clock 1 = em close ({em:null}), clock 2 = strong open, clock 3 = strong close.
/// `acceptChanges(createID(1,1), createID(1,1))` accepts ONLY the em CLOSE marker (clock 1),
/// splitting the em pair. The accept itself still renders consistently — the residual is armed,
/// not visible. The next suggested unformat of 'A' then corrupts the maintained cache: the
/// incremental render lets the strong suggestion's format attribution run past its close marker
/// onto the trailing 'B':
///
///   cached: A(strong, {format:{strong:[]}}) | B({format:{strong:[]}} <- STALE)
///   fresh : A(strong, {format:{strong:[]}}) | B
///
/// Load-bearing (each verified): the accept range must contain the em close marker and NOT the
/// em open marker (1-1, 1-2, 1-3 all drift; 0-0, 0-1 whole-pair, 2-2, 3-3 all clean) — the
/// marker-pair split IS the trigger; a second suggested format with a different key and non-null
/// value (same key em -> clean, strong:null -> clean); a trailing char after the formatted one
/// (init 'A' alone -> clean, formatting the last char -> clean); the final op must be an
/// unformat — em:{} reformat is clean, while em:null and strong:null both drift identically.
/// Every step before the final unformat is consistent, including the accept. Both clientID
/// orders fail byte-identically; the accept does not echo structs back (no Math.random clientID
/// reassignment), so the repro is seed-free: 100/100 identical failing runs. Same stale-
/// {format:{strong:[]}}-on-trailing-char symptom as the (fixed)
/// testRdtRejectAllOverlappingSameKeyFormatProvenanceResidual pin, but a distinct class: no base
/// format involved, the trigger is a marker-splitting PARTIAL accept, and the drift is deferred
/// to a later unformat.
#[test]
fn split_format_marker_pair_partial_accept_then_unformat() {
    let (prev, next) = docs(0, 1);
    let s = session(&prev, &next);
    let (em, strong) = (fmt("em", any!({})), fmt("strong", any!({})));
    apply(&prev, "t", Delta::new().insert_text("AB"));
    // clock 0/1 = em open/close, clock 2/3 = strong open/close
    suggest(&s, &next, "t", Delta::new().retain_fmt(1, &em));
    suggest(&s, &next, "t", Delta::new().retain_fmt(1, &strong));
    let both = Attrs::from([("em".into(), any!({})), ("strong".into(), any!({}))]);
    let a = || Delta::new().insert_text_attributed("A", Some(&both), formatted(&["em", "strong"]));
    assert_eq!(render(&s, &next, "t"), a().insert_text("B"));
    // accept ONLY the em close marker
    let em_close = ID::new(ClientID::new(1), 1);
    s.accept_changes(em_close, Some(em_close));
    let b = Delta::new().insert_text_attributed("B", None, formatted(&["em", "strong"]));
    assert_eq!(render(&s, &next, "t"), a().join(b));
    assert_eq!(content(&prev, "t"), Delta::new().insert_text("AB"));
    suggest(
        &s,
        &next,
        "t",
        Delta::new().retain_fmt(1, &fmt("em", Any::Null)),
    );
    // t.assert(cached.equals(fresh))
    let expected = Delta::new()
        .insert_text_attributed("A", Some(&strong), formatted(&["strong"]))
        .insert_text("B");
    assert_eq!(render(&s, &next, "t"), expected);
}

/// Yjs `testRdtPartialAcceptSkippingUnformatOpenMarkerCacheDrift`:
/// Regression pin — fixed by the same reset-branch clear re-add as the split-marker-pair class
/// above (both are the marker-pair-splitting-accept walk shape). Minimized from lifecycle-fuzz
/// seed 1034 (failIndex 15, 16 ops → 6):
/// PARTIAL-ACCEPT-triggered LATENT cache drift, stale-attribution direction. A committed base
/// `strong` over 'abc'; a suggested unformat (strong → null) of 'b' (sugg-client markers:
/// clock 0 = open strong:null, clock 1 = close/restore strong:{}); a second suggested format
/// with a DIFFERENT key (em:{}) on the same 'b' (clocks 2/3); then a partial
/// `acceptChanges(id(1,1), id(1,1))` that accepts ONLY the unformat's range-END restore marker,
/// skipping its open marker at clock 0. The accept itself still renders consistently — the
/// corruption is latent — but the next suggested format edit over the region (removing the em)
/// re-renders 'c' with a stale `{format:{strong:[]}}` the fresh deep render does not have:
///
///   cached: a(strong) | b({format:{strong:[]}}) | c(strong, {format:{strong:[]}} ← STALE)
///   fresh : a(strong) | b({format:{strong:[]}}) | c(strong)
///
/// Load-bearing: the accept range must EXCLUDE clock 0 and include at least one later sugg
/// marker — accept(1,1), accept(2,2) and accept(3,3) all drift identically, while accept(0,0),
/// accept(0,3), acceptAll and reject(1,1) render clean; the second suggested format must exist
/// BEFORE the accept and use a different key (dropping it, or using strong for both, renders
/// clean — which op carries which key does not matter, swapped keys drift too); the final op
/// must be a format CHANGE re-rendering the region (em:null on 'b' or 'c', or a base em:{} on
/// 'b' — re-applying the same em:{}, inserts, deletes and clearCache all render clean); and the
/// committed strong must extend both strictly before and strictly past 'b' (2-char docs 'ab'/'bc'
/// and base ranges not covering 'a' or 'c' render clean). Both clientID orders drift identically;
/// the accept does not echo structs back (clientIDs survive unreassigned), so the repro is
/// Math.random-free: 100/100 identical failing runs without seeding, on both trees. Related to
/// (but distinct from) the fixed testRdtAcceptAllOfSameValueSuggestedFormatCacheDrift: here
/// acceptAll is CLEAN, the partial id range is essential, and the drift needs a second op after
/// the accept.
#[test]
fn partial_accept_skipping_unformat_open_marker() {
    let (prev, next) = docs(0, 1);
    let s = session(&prev, &next);
    let (em, strong) = (fmt("em", any!({})), fmt("strong", any!({})));
    apply(&prev, "t", Delta::new().insert_text("abc"));
    apply(&prev, "t", Delta::new().retain_fmt(3, &strong));
    // clock 0 = open strong:null, clock 1 = close strong:{}
    let unbold = fmt("strong", Any::Null);
    suggest(
        &s,
        &next,
        "t",
        Delta::new().retain(1).retain_fmt(1, &unbold),
    );
    // clock 2 = em:{}, clock 3 = em:null
    suggest(&s, &next, "t", Delta::new().retain(1).retain_fmt(1, &em));
    let ab = || {
        let b = formatted(&["strong", "em"]);
        let d = Delta::new().insert_text_fmt("a", &strong);
        d.insert_text_attributed("b", Some(&em), b)
    };
    assert_eq!(render(&s, &next, "t"), ab().insert_text_fmt("c", &strong));
    // accept ONLY the unformat's close marker
    let close = ID::new(ClientID::new(1), 1);
    s.accept_changes(close, Some(close));
    let c = formatted(&["strong", "em"]);
    let expected = ab().insert_text_attributed("c", Some(&strong), c);
    assert_eq!(render(&s, &next, "t"), expected);
    assert_eq!(
        content(&prev, "t"),
        Delta::new().insert_text_fmt("abc", &strong)
    );
    suggest(
        &s,
        &next,
        "t",
        Delta::new().retain(1).retain_fmt(1, &fmt("em", Any::Null)),
    );
    // t.assert(cached.equals(fresh))
    let expected = Delta::new()
        .insert_text_fmt("a", &strong)
        .insert_text_attributed("b", None, formatted(&["strong"]))
        .insert_text_fmt("c", &strong);
    assert_eq!(render(&s, &next, "t"), expected);
}

/// Random text, node, format and attribute ops on a root or a nested node.
fn random_op(doc: &Cell<Doc>, rng: &mut fastrand::Rng) {
    let mut doc = doc.acquire_mut();
    let mut txn = doc.transact_mut();
    let mut ids = vec![txn.node("root").unwrap().id()];
    let mut i = 0;
    while i < ids.len() {
        let node = txn.node(ids[i].clone()).unwrap();
        for value in node.iter() {
            ids.extend(value.node_id());
        }
        i += 1;
    }
    let id = ids[rng.usize(..ids.len())].clone();
    let mut node = txn.node_mut(id).unwrap();
    let len = node.len();
    let word = |rng: &mut fastrand::Rng| -> String {
        (0..rng.usize(1..4)).map(|_| rng.lowercase()).collect()
    };
    match rng.u32(0..5) {
        0 => {
            let at = rng.u32(0..=len);
            node.insert_text(at, &word(rng));
        }
        1 => {
            node.insert(rng.u32(0..=len), Delta::new());
        }
        2 if len > 0 => {
            let at = rng.u32(0..len);
            node.remove(at, rng.u32(1..=(len - at).min(3)));
        }
        3 if len > 0 => {
            let at = rng.u32(0..len);
            let bold = if rng.bool() {
                Any::Bool(true)
            } else {
                Any::Null
            };
            node.format(at, rng.u32(1..=(len - at).min(3)), &fmt("bold", bold));
        }
        4 => {
            let key = ["a", "b", "c"][rng.usize(..3)];
            if rng.bool() {
                node.insert_attr(key, word(rng));
            } else {
                node.remove_attr(key);
            }
        }
        _ => {}
    }
}

/// Fuzz under a diffing renderer, across two synced replicas. Each replica is a "suggestion doc" that
/// diffs against its own fixed baseline clone (taken after some shared initial content). With the plain
/// diff renderer (no `attrs`), suggestion inserts render `{ insert: [] }` and deletes render
/// `{ delete: [] }` — identical on every replica — so the maintained, diff-attributed `delta` must
/// converge across replicas (and match a fresh deep render). The cache is kept current purely by the
/// `'delta'` event (no recompute). Also memo-pressures both replicas' live caches via fingerprint
/// reads each iteration (see `assertDeltaCacheFresh`) — these consume no prng, so seeds reproduce.
#[test]
fn delta_suggestion_convergence() {
    let docs: Vec<_> = (1..=2).map(|i| Cell::new(Doc::with_client_id(i))).collect();
    let d = Delta::new().insert_text("shared baseline content");
    apply(&docs[0], "root", d);
    exchange_updates(&mut [&mut *docs[0].acquire_mut(), &mut *docs[1].acquire_mut()]);
    // each replica diffs against its own fixed baseline clone
    let bases: Vec<_> = docs.iter().map(|d| clone_doc(&d.acquire(), None)).collect();
    let sessions: Vec<_> = (0..2)
        .map(|i| {
            let base = Cell::new(clone_doc(&bases[i], None));
            Suggestion::new(base, docs[i].clone(), None::<ContentMap>)
        })
        .collect();
    let mut rng = fastrand::Rng::with_seed(1);
    for i in 0..300 {
        random_op(&docs[rng.usize(..2)], &mut rng);
        exchange_updates(&mut [&mut *docs[0].acquire_mut(), &mut *docs[1].acquire_mut()]);
        let a = render(&sessions[0], &docs[0], "root");
        let b = render(&sessions[1], &docs[1], "root");
        assert_eq!(a, b, "converge iter {i}");
        // the live formatter matches a fresh one (yrs has no maintained `.delta` cache)
        let doc = docs[0].acquire();
        let mut fresh = DiffFormatter::unattributed(&ContentIds::doc_diff(&bases[0], &doc));
        fresh.restore_content(&bases[0].transact());
        let txn = doc.transact();
        let fresh = txn
            .node("root")
            .unwrap()
            .formatted(&fresh)
            .delta(&DeltaOptions::deep());
        assert_eq!(a, fresh.into(), "fresh iter {i}");
    }
}

#[test]
fn suggestion_origins() {
    let (prev, next) = docs(1, 2);
    let s = session(&prev, &next);
    s.set_suggestion_mode(false);
    s.set_suggestion_origins(Some(vec!["user".into()]));
    apply(&next, "u", Delta::new().insert_text("a"));
    assert_eq!(content(&prev, "u"), Delta::new()); // no origin, not applied
    let mut doc = next.acquire_mut();
    doc.transact_mut_with("user")
        .node_mut("t")
        .unwrap()
        .insert_text(0, "b");
    drop(doc);
    let b = Delta::new().insert_text("b");
    assert_eq!(content(&prev, "t"), b); // tracked origin gets applied
}

#[test]
fn reject_gc_deleted_suggestion() {
    let prev = Cell::new(Doc::with_client_id(1));
    let next = Cell::new(Doc::with_options(Options {
        cleanup_formatting: false,
        ..Options::with_client_id(ClientID::new(2))
    }));
    let s = session(&prev, &next);
    apply(&prev, "t", Delta::new().insert_text("hello world")); // 'hello world'
    suggest(&s, &next, "t", Delta::new().retain(6).remove(5)); // 'hello '
    let next_state = next
        .acquire_mut()
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    // `next` no longer holds 'world' - it should have been GCed
    assert!(!next_state.windows(b"world".len()).any(|s| s == b"world"));

    let deleted = Some(Attribution::delete::<Attribute>([]));
    let struck = Delta::new()
        .insert_text("hello ")
        .insert_text_attributed("world", None, deleted);
    assert_eq!(render(&s, &next, "t"), struck); // we still can render: 'hello ~world~'

    let w = ID::new(ClientID::new(1), 6);
    s.reject_changes(w, Some(w)); // reject changes from `next`, bring back 'world'
    let expected = Delta::new().insert_text("hello world");
    assert_eq!(content(&next, "t"), expected);
    assert_eq!(content(&prev, "t"), expected);
    assert_eq!(render(&s, &next, "t"), expected);
}

/// The session can be shared across threads.
#[cfg(feature = "sync")]
#[test]
fn suggestion_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<Suggestion>();
}
