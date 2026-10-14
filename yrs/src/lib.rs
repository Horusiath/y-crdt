#![doc(
    html_logo_url = "https://raw.githubusercontent.com/y-crdt/y-crdt/main/logo-yrs.svg",
    html_favicon_url = "https://raw.githubusercontent.com/y-crdt/y-crdt/main/logo-yrs.svg"
)]

//! Yrs (read: "wires") is a high performance CRDT implementation based on the idea of **Shared Types**.
//! It is a compatible port of the [Yjs](https://github.com/yjs/yjs) CRDT.
//!
//! **Shared Types** work just like normal data types, but they automatically sync with other peers.
//!
//! A **Document** is the access point to create shared types, and to listen to update events.
//!
//! # Features
//!
//! - `small-client` this feature is used for compatibility with older versions of
//!    yrs (v0.26 and below) and yjs (pre v14). It uses 32bit [crate::ClientID]. The current default
//!    uses 53-bit (max safe integer number in JavaScript/Yjs).
//! - `weak` this feature enables weak references and quotations (see: [crate::WeakRef]).
//! - `sync` this feature modifies observers callback constraints to use `Send` and `Sync` traits.
//!   These are required when using yrs features in multithreaded environments.
//!
//! # Quick start
//!
//! Let's discuss basic features of Yrs. We'll introduce these concepts starting from
//! the following code snippet:
//!
//! ```rust
//! use yrs::{Doc, StateVector, Update};
//! use yrs::lib0::{Decode, Encode};
//!
//! let mut doc = Doc::new();
//!
//! {
//!   let mut txn = doc.transact_mut();
//!   let mut text = txn.node_mut("article").unwrap();
//!   text.insert_text(0, "hello");
//!   text.insert_text(5, " world");
//!   // other rich text operations include formatting or inserting embedded elements
//! } // transaction is automatically committed when dropped
//!
//! assert_eq!(doc.transact().node("article").unwrap().to_string(), "hello world");
//!
//! // synchronize state with remote replica
//! let mut remote_doc = Doc::new();
//! let remote_timestamp = remote_doc.transact().state_vector().encode_v1();
//!
//! // get update with contents not observed by remote_doc
//! let update = doc.transact().encode_diff_v1(&StateVector::decode_v1(&remote_timestamp).unwrap());
//! // apply update on remote doc
//! remote_doc.transact_mut().apply_update(Update::decode_v1(&update).unwrap()).unwrap();
//!
//! assert_eq!(remote_doc.transact().node("article").unwrap().to_string(), "hello world");
//! ```
//!
//! [Doc] is a core structure of Yrs. All other structures and operations are performed in
//! context of their document. All documents gets randomly generated [Doc::client_id] (which can be
//! also defined explicitly), which must be unique per active peer. It's crucial, as potential
//! concurrent changes made by different peers sharing the same [ClientID] will cause a document
//! state corruption.
//!
//! Next line defines [TextRef] - a shared collection specialized in providing collaborative rich text
//! operations. In snippet above it has been defined by calling [Doc::get_or_insert_text] method.
//! Shared types defined at the document level are so called root types. Root level types sharing
//! the same name across different peers are considered to be replicas of the same logical entity,
//! regardless of their type. It's highly recommended for all collaborating clients to define all
//! root level types they are going to use up front, during document creation. A list of supported
//! shared types include: [TextRef], [ArrayRef], [MapRef], [XmlTextRef], [XmlFragmentRef] and [XmlElementRef].
//!
//! Next section of code performs an update over the defined text replica. In Yrs all operations
//! must be executed in a scope of transaction created by the document they were defined in. We
//! can differentiate two transaction types:
//!
//! 1. [Read-only transactions](Transaction), created via [Transact::transact]/[Transact::try_transact].
//!    They are used only to access the contents of an underlying document store but they never
//!    alter it. They are useful for methods like reading the structure state or for serialization.
//!    It's allowed to have multiple active read-only transactions as long as no read-write
//!    transaction is in progress.
//! 2. [Read-write transactions](TransactionMut), create via [Transact::transact_mut]/[Transact::try_transact_mut].
//!    These can be used to modify the internal document state. These transactions work as
//!    intelligent batches. They are automatically committed when dropped, performing tasks like
//!    state cleaning, metadata compression and triggering event callbacks. Read-write transactions
//!    require exclusive access to an underlying document store - no other transaction (neither
//!    read-write nor read-only one) can be active while read-write transaction is to be created.
//!
//! In order to synchronize state between the document replicas living on a different peer processes,
//! there are two possible cases:
//!
//! 1. Peer who wishes to receive an update first encodes its document's [state vector](Transaction::state_vector).
//!    It's a logical timestamp describing which updates that has been observed by this document instance
//!    so far. This [StateVector] can be later serialized and passed to the remote collaborator. This
//!    collaborator can then deserialize it back and [generate an update](Transaction::encode_diff) which
//!    will contain all new changes performed since provided state vector. Finally this update can
//!    be passed back to the requester, [deserialized](Update::decode) and integrated into a document
//!    store via [TransactionMut::apply_update].
//! 2. Another propagation mechanism relies on subscribing to [Doc::observe_update_v1] or
//!    [Doc::observe_update_v2] events, which will be fired whenever an referenced document will
//!    detect new changes.
//!
//! While the 2nd option can produce smaller binary payload than 1st one at times and doesn't require
//! request-response cycles, it cannot pass the document state prior the observer callback
//! registration and expects that all changes will surely be delivered to other peer. A practical
//! approach (used i.e. by [y-sync protocol](https://crates.io/crates/y-sync)) is usually a combination
//! of both variants: use 1st one on connection initialization between two peers followed by
//! 2nd approach to deliver subsequent changes.
//!
//! # Formatting and embedding
//!
//! While the quick start example covered only a simple text insertions, structures such as
//! [TextRef]/[XmlTextRef] are capable of including more advanced operators, such as adding
//! [formatting attributes](Text::format), [inserting embedded content](Text::insert_embed)
//! (eg. image binaries or [ArrayRef]s that we could interpret in example as nested tables).
//!
//! ```rust
//! use yrs::{Any, Delta, Doc};
//! use yrs::node::Attrs;
//!
//! let mut doc = Doc::new();
//! let mut txn = doc.transact_mut();
//! let mut text = txn.node_mut("article").unwrap();
//!
//! let bold = Attrs::from([("b".into(), true.into())]);
//! let italic = Attrs::from([("i".into(), true.into())]);
//!
//! text.insert_text(0, "hello ");
//! text.insert_text_fmt(6, "world", &italic);
//! text.format(0, 5, &bold);
//!
//! assert_eq!(text.format_xml().to_string(), "<b>hello</b> <i>world</i>");
//!
//! // remove formatting
//! let remove_italic = Attrs::from([("i".into(), Any::Null)]);
//! text.format(6, 5, &remove_italic);
//!
//! assert_eq!(text.format_xml().to_string(), "<b>hello</b> world");
//!
//! // insert binary payload eg. images
//! let image = b"deadbeaf".to_vec();
//! text.insert(1, image);
//!
//! // insert nested node eg. table as a list of rows
//! let table = Delta::new()
//!     .insert(Delta::new().insert("Book title").insert("Author"))
//!     .insert(Delta::new().insert("\"Moby-Dick\"").insert("Herman Melville"));
//! text.insert(5, table);
//! ```
//!
//! Keep in mind that this kind of special content may not be displayed using standard methods
//! ([TextRef::get_string] returns only inserted text and ignores other content, while
//! [XmlTextRef::get_string] renders formatting attributes as XML nodes, but still ignores embedded
//! values). Reason behind this behavior is that as generic collaboration library, Yrs cannot make
//! opinionated decisions in this regard - whenever a full collection of text chunks, formatting
//! attributes or embedded items is required, use [Text::diff] instead.
//!
//! # Cursor positioning
//!
//! Another common problem of collaborative text editors is a requirement of keeping track of cursor
//! position in the face of concurrent updates incoming from remote peers. Let's present the problem
//! on following example:
//!
//! ```rust
//! use yrs::{Doc, StateVector, Update};
//! use yrs::lib0::Decode;
//!
//! let mut doc1 = Doc::with_client_id(1);
//! let mut txn1 = doc1.transact_mut();
//! txn1.node_mut("article").unwrap().insert_text(0, "hello");
//!
//! let mut doc2 = Doc::with_client_id(2);
//! let mut txn2 = doc2.transact_mut();
//! txn2.node_mut("article").unwrap().insert_text(0, "world");
//!
//! const INDEX: usize = 1;
//!
//! // Doc 2: cursor at index 1 points to character 'o'
//! let str = txn2.node("article").unwrap().to_string();
//! assert_eq!(str.chars().nth(INDEX), Some('o'));
//!
//! // synchronize full state of doc1 -> doc2
//! txn2.apply_update(Update::decode_v1(&txn1.encode_diff_v1(&StateVector::default())).unwrap()).unwrap();
//!
//! // Doc 2: cursor at index 1 no longer points to the same character
//! let str = txn2.node("article").unwrap().to_string();
//! assert_ne!(str.chars().nth(INDEX), Some('o'));
//! ```
//!
//! Since [TransactionMut::apply_update] merges updates performed by remote peer, some of these
//! them may shift the cursor position. However in such case the old index that we used (`1` in the
//! example above) is no longer valid.
//!
//! To address these issues, we can make use of [StickyIndex] struct to save the permanent
//! location, that will persist between concurrent updates being made:
//!
//! ```rust
//! use yrs::{Assoc, Doc, StateVector, Update};
//! use yrs::lib0::Decode;
//!
//! let mut doc1 = Doc::with_client_id(1);
//! let mut txn1 = doc1.transact_mut();
//! txn1.node_mut("article").unwrap().insert_text(0, "hello");
//!
//! let mut doc2 = Doc::with_client_id(2);
//! let mut txn2 = doc2.transact_mut();
//! txn2.node_mut("article").unwrap().insert_text(0, "world");
//!
//! const INDEX: usize = 1;
//!
//! // Doc 2: cursor at index 1 points to character 'o'
//! let text2 = txn2.node("article").unwrap();
//! assert_eq!(text2.to_string().chars().nth(INDEX), Some('o'));
//!
//! // get a permanent index for cursor at index 1
//! let pos = text2.sticky_index(INDEX as u32, Assoc::After).unwrap();
//!
//! // synchronize full state of doc1 -> doc2
//! txn2.apply_update(Update::decode_v1(&txn1.encode_diff_v1(&StateVector::default())).unwrap()).unwrap();
//!
//! // restore the index from position saved previously
//! let idx = pos.get_offset(&txn2).unwrap();
//! let str = txn2.node("article").unwrap().to_string();
//! assert_eq!(str.chars().nth(idx.index as usize), Some('o'));
//! ```
//!
//! [StickyIndex] structure is serializable and can be persisted or passed over the network as
//! well, which may help with tracking and displaying the cursor location of other peers.
//!
//! # Weak links and quotations
//!
//! This functionality requires a "weak" feature flag to be turned on:
//! ```toml
//! yrs = { version = "0.17", features = ["weak"] }
//! ```
//!
//! Yrs document structure can be represented as a tree of elements. That means that usually a node
//! can have only one parent and cannot be referenced by any other node. This can be changed by
//! usage of weak links - they offer you a way to reference to values existing in other parts of
//! the document (also other collections):
//!
//! - [Map] elements can be references via [Map::link] method.
//! - Other collections like text and arrays, can quote entire ranges of values via [Quotable::quote]
//!   method.
//!
//! Both of these methods return a [WeakPrelim] struct can be integrated as an input value in other
//! collections and convert into [WeakRef] shared type.
//!
//! ```rust
//! #[cfg(feature = "weak")]
//! fn example() {
//!     use yrs::{Delta, Doc};
//!
//!     let mut doc = Doc::new();
//!     let mut txn = doc.transact_mut();
//!     let mut text = txn.node_mut("text").unwrap();
//!     text.insert_text(0, "hello!");
//!     let quote = text.quote(0..5).unwrap();
//!     let quote = txn
//!         .node_mut("map")
//!         .unwrap()
//!         .insert_attr("title", Delta::link(quote))
//!         .node_id()
//!         .unwrap();
//!
//!     // retrieve quoted text fragment
//!     assert_eq!(txn.node(quote.clone()).unwrap().to_string(), "hello");
//!
//!     // quotations are actively reacting to changes happening at source within quoted range
//!     txn.node_mut("text").unwrap().insert_text(5, " world");
//!     // since quoted range 0..5 was right-side exclusive, index 5 itself is not included in range,
//!     // but inserts between position 4 and 5 are
//!     assert_eq!(txn.node(quote).unwrap().to_string(), "hello world");
//! }
//! ```
//!
//! Weak refs also expose observer API that allows to subscribe to changes happening in source
//! collections within quoted range.
//!
//! Keep in mind that weak refs don't maintain ownership over quoted elements. If a source
//! collection removes a quoted element, it will no longer be accessible from weak ref:
//!
//! ```rust
//! #[cfg(feature = "weak")]
//! fn example() {
//!     use yrs::{Delta, Doc};
//!
//!     let mut doc = Doc::new();
//!     let mut txn = doc.transact_mut();
//!     let mut map = txn.node_mut("map").unwrap();
//!     map.insert_attr("origin", "value");
//!     // establish a link 'origin' entry
//!     let link = map.link("origin").unwrap();
//!     let link = map.insert_attr("link", Delta::link(link)).node_id().unwrap();
//!     let linked_value = txn.node(link.clone()).unwrap().try_deref();
//!     assert_eq!(linked_value, Some("value".into()));
//!
//!     // remove original value
//!     txn.node_mut("map").unwrap().remove_attr("origin");
//!     let linked_value = txn.node(link).unwrap().try_deref();
//!     assert_eq!(linked_value, None); // linked value is no longer accessible
//! }
//! ```
//!
//! # Undo/redo
//!
//! Among very popular features of many user-facing applications is an ability to revert/reapply
//! operations performed by user. This becomes even more complicated, once we consider multiple peers
//! collaborating on the same document, as we may need to skip over the changes synchronized from
//! remote peers - even thou they could have happened later - in order to only undo our own actions.
//! [UndoManager] is a Yrs response for these needs, supporting wide variety of options:
//!
//! ```rust
//! use yrs::{Acquire, AcquireMut, Cell, Doc, UndoManager, Update};
//! use yrs::lib0::Decode;
//! use yrs::undo::Options;
//!
//! let local = Cell::new(Doc::with_client_id(123));
//! let local_client_id = local.acquire().client_id();
//! let mut mgr = UndoManager::with_options(Options::default());
//! mgr.expand_scope(&mut local.transact_owned_mut().node_mut("article").unwrap());
//! mgr.include_origin(local_client_id); // only track changes originating from local peer
//!
//! let remote = Cell::new(Doc::with_client_id(321));
//! let remote_client_id = remote.acquire().client_id();
//!
//! let push_text = |doc: &Cell<Doc>, origin, text: &str| {
//!     let mut doc = doc.acquire_mut();
//!     let mut txn = doc.transact_mut_with(origin);
//!     txn.node_mut("article").unwrap().push_text(text);
//! };
//! let read_text = |doc: &Cell<Doc>| doc.acquire().transact().node("article").unwrap().to_string();
//!
//! // perform changes locally
//! push_text(&local, local_client_id, "hello ");
//! mgr.reset(); // prevent previous and next operation to be treated by Undo manager as one batch
//! push_text(&local, local_client_id, "world");
//! assert_eq!(read_text(&local), "hello world");
//!
//! // perform remote changes - these are not being tracked by mgr
//! push_text(&remote, remote_client_id, "everyone");
//! assert_eq!(read_text(&remote), "everyone");
//!
//! // sync changes from remote to local
//! let update = remote.acquire().transact().encode_state_as_update_v1(&local.acquire().transact().state_vector());
//! local.acquire_mut().transact_mut().apply_update(Update::decode_v1(&update).unwrap()).unwrap();
//! assert_eq!(read_text(&local), "hello worldeveryone"); // remote changes synced
//!
//! // undo last performed change on local
//! mgr.undo_blocking();
//! assert_eq!(read_text(&local), "hello everyone");
//!
//! // redo change we undone
//! mgr.redo_blocking();
//! assert_eq!(read_text(&local), "hello worldeveryone");
//! ```
//!
//! > Keep in mind, that in order to serve its purpose, undo manager may need to implicitly create
//! transactions over underlying [Doc] - for that reason make sure that no other transaction is
//! active while calling methods like [UndoManager::undo] or [UndoManager::redo].
//!
//! It's important to understand a context, in which [UndoManager] operates:
//!
//! - **origins** can be specific classifiers attached to read-write transactions upon creation (see:
//!   [Transact::transact_mut_with]). Undo manager can [include](UndoManager::include_origin) any
//!   number of origins to its track scope. By default no origin is specified: in such case undo
//!   manager will track all changes without looking at transaction origin.
//! - **scope** refers to shared collection references such as [TextRef], [MapRef] etc. Undo manager
//!   will only track operations performed on tracked scope refs. By default at least one such
//!   reference must be specified, but they can be [expanded](UndoManager::expand_scope) to include
//!   multiple collections at once if necessary.
//!
//! Notice, that undo/redo changes are **not** mapped 1-1 on the update batches committed by
//! transactions. As an example: a frequent case includes establishing a new transaction for every
//! user key stroke. Meanwhile we may decide to use different granularity of undo/redo actions.
//! These are grouped together on time-based ranges (configurable in [undo::Options], which is
//! 500ms by default). You can also set them explicitly by calling [UndoManager::reset] method.
//!
//! # Showing past revisions of the document
//!
//! Another feature of Yrs is an ability to snapshot and recover versions of the document,
//! as well as show the differences between them:
//!
//! ```rust
//! use yrs::{Doc, Options, Update};
//! use yrs::lib0::{Decode, Encoder, EncoderV1};
//! use yrs::node::Attrs;
//!
//! let mut doc = Doc::with_options(Options {
//!     skip_gc: true,  // in order to support revisions we cannot garbage collect deleted blocks
//!     ..Options::default()
//! });
//! let mut txn = doc.transact_mut();
//!
//! const INIT: &str = "hello world";
//! txn.node_mut("article").unwrap().push_text(INIT);
//!
//! // save current state of the document: "hello world"
//! let rev = txn.snapshot();
//!
//! // change document state
//! let italic = Attrs::from([("i".into(), true.into())]);
//! let mut text = txn.node_mut("article").unwrap();
//! text.format(6, 5, &italic);
//! text.remove(2, 3);
//! assert_eq!(text.format_xml().to_string(), "he <i>world</i>");
//!
//! // encode past version of the document
//! let mut encoder = EncoderV1::new();
//! txn.encode_state_from_snapshot(&rev, &mut encoder).unwrap();
//! let update = encoder.to_vec();
//!
//! // restore the past state
//! let mut doc = Doc::new();
//! let mut txn = doc.transact_mut();
//! txn.apply_update(Update::decode_v1(&update).unwrap()).unwrap();
//!
//! assert_eq!(txn.node("article").unwrap().to_string(), INIT);
//! ```
//!
//! Keep in mind that an update created from past snapshot via [TransactionMut::encode_state_from_snapshot]
//! doesn't contain updates that happened after that snapshot. What does that mean? While you can
//! continue making new updates on top of that revision, they will be no longer compatible with any
//! changes made on the original document since the snapshot has been made, therefore [TransactionMut::apply_update]
//! on updates generated between two document revisions that branched their state is no longer possible.
//!
//! For the reason above main use case of this feature is rendering read-only state of the [Doc]
//! or restoring document from its past state ie. when current state has been irrecoverably corrupted.
//!
//! # Other shared types
//!
//! So far we only discussed rich text oriented capabilities of Yrs. However, it's possible to make
//! use of Yrs to represent any tree-like object:
//!
//! - [ArrayRef] can be used to represent any indexable sequence of values. If there are multiple
//!   peers inserting values at the same position, a [ClientID] will be used to determine a final
//!   deterministic order once all peers get in sync.
//! - [MapRef] is a map object (with keys limited to be strings), where values can be of any given
//!   type. If there are multiple peers updating the same entry concurrently - creating an update
//!   conflict in the result - Yrs will prioritize update belonging to a peer with higher [ClientID]
//!   to make conflict resolution algorithm deterministic.
//! - Yrs also provides support fo XML nodes in form of [XmlElementRef], [XmlTextRef] and [XmlFragmentRef].
//!
//! Underneath all of these types are represented by the same abstract [types::Node] type. Each
//! branch is always capable of working as both indexed sequence of elements and a map. In practice
//! specialized shared types are actually projections over branch type and can be used interchangeably
//! if needed, i.e.: [XmlElementRef] can be also interpreted as [MapRef], in which case the collection
//! of that XML node attributes become key-value entries of reinterpreted map's.
//!
//! # Preliminary vs Integrated types
//!
//! In Yrs core library, every shared type has 2 representations:
//!
//! - **Integrated type** (eg. [TextRef], [ArrayRef], [MapRef]) represents a reference that has already
//!   been attached to its parent [Doc]. As such, its state is tracked as part of that document, it
//!   can be modified concurrently by multiple peers and any conflicts that occurred due to such
//!   actions will be automatically resolved accordingly to [YATA conflict resolution algorithm](https://www.researchgate.net/publication/310212186_Near_Real-Time_Peer-to-Peer_Shared_Editing_on_Extensible_Data_Types).
//! - **Preliminary type** (eg. [TextPrelim], [ArrayPrelim], [MapPrelim]) represents a content that
//!   we want to eventually turn into an integrated reference, but it has not been integrated yet.
//!
//! Whenever we want to nest shared types one into another - using methods such as [Array::insert],
//! [Map::insert] or [Text::insert_embed] - we always must do so using preliminary types. These
//! methods will return an integrated representation of the preliminary content we wished to integrate.
//!
//! Keep in mind that we cannot integrate references that have been already integrated - neither in
//! the same document nor in any other one. Yjs/Yrs doesn't allow to have the same object to be linked
//! in multiple places. Same rule concerns primitive types (they will eventually be serialized and
//! deserialized as unique independent objects), integrated types or sub-documents.
//!
//! # Shared collection hooks
//!
//! While type like [TextRef], [MapRef] etc. refer to addresses of objects living in memory,
//! sometime you'd like to be able to uniquely identify the same collection across its different
//! replicas living on other peers. This is possible via hooks:
//!
//! ```rust
//! use yrs::{Delta, Doc, NodeID, Update};
//! use yrs::lib0::Decode;
//!
//! // root nodes are identified by their name
//! let root = NodeID::from("root");
//!
//! let mut local = Doc::with_client_id(1);
//! let nested = local
//!     .transact_mut()
//!     .node_mut(root)
//!     .unwrap()
//!     .push_back(Delta::new().insert_attr("key", "old"))
//!     .node_id()
//!     .unwrap(); // logical identifier of a nested node
//!
//! let mut remote = Doc::with_client_id(2);
//! // we haven't synchronized yet, so nested element doesn't exist on remote
//! assert!(remote.transact().node(nested.clone()).is_none());
//!
//! // synchronize the changes
//! {
//!     let mut txn = remote.transact_mut();
//!     let update = local.transact().encode_state_as_update_v1(&txn.state_vector());
//!     txn.apply_update(Update::decode_v1(&update).unwrap()).unwrap();
//! }
//!
//! // after synchronizing, we can now access the same logical node on the remote peer
//! let (local_txn, remote_txn) = (local.transact(), remote.transact());
//! let local_map = local_txn.node(nested.clone()).unwrap();
//! let remote_map = remote_txn.node(nested).unwrap();
//! assert_eq!(local_map.id(), remote_map.id());
//! assert_eq!(local_map.to_json(), remote_map.to_json());
//! ```
//!
//! Logical references are serializable. They also can help to avoid segfaults in cases when we hold
//! an unsafe reference to a nested collection that has been already deleted by concurrent operation.
//!
//! # Transaction event lifecycle
//!
//! Yrs provides a variety of lifecycle events, which enable users to react on various situations
//! and changes performed. Some of these events are used by Yrs own features (e.g. [UndoManager]).
//! They are always triggered once performed update is committed by dropping or
//! [committing](TransactionMut::commit) a read-write transaction.
//!
//! An order in which these updates are fired is as follows:
//!
//! 1. Observers on updated shared types: [TextRef::observe], [ArrayRef::observe], [MapRef::observe],
//!    [XmlTextRef::observe], [XmlFragmentRef::observe] and [XmlElementRef::observe].
//! 2. [Deep observers](DeepObservable::observe_deep) (special kind of observers that are bubbled up
//!    from nested shared types through their parent collections hierarchy).
//! 3. After transaction callbacks: [Doc::observe_after_transaction].
//! 4. After transaction cleanup callbacks (moment after all changes performed by transaction have
//!    been compressed an integrated into document store): [Doc::observe_transaction_cleanup].
//! 5. Update callbacks: [Doc::observe_update_v1] and [Doc::observe_update_v2]. Useful when we want
//!    to encode and propagate incremental changes made by transaction to other peers.
//! 6. Sub-document change callbacks: [Doc::observe_subdocs].
//!
//! # Update lib0 v1 vs. v2
//!
//! Yrs ships with so called lib0 codecs, which offers two different variants, both of which are
//! compatible with [Yjs](https://docs.yjs.dev/) and can be used to bridge between applications
//! written in different languages using Yrs underneath. They offer a highly compact format,
//! that aims to produce small binary payload size and high lib0/decoding speed.
//!
//! V1 lib0 is the default one and should be preferred most of the time. V2 lib0 aims to
//! perform payload size optimizations whenever multiple updates are being encoded and passed
//! together (e.g. when you want to serialize an entire document state), however for small updates
//! (e.g. sending individual user keystrokes) V2 may turn out to be less optimal than its V1
//! equivalent.
//!
//! # Awareness and y-sync protocol
//!
//! Sometimes it's not always feasible to keep all state changes related to a document within a
//! document update history - especially if they are notifications bound to a single user or session.
//! For this reason most of the editor bindings using Yjs supplement the document state with
//! [sync::Awareness] - it's a separate entity that keeps user related data such as username
//! or cursor position, but without the overhead of keeping metadata necessary for conflict
//! resolution.
//!
//! [sync::Awareness] together with [Doc] updates combined create basis of y-sync [sync::Protocol].
//! It's used by most of the network providers in Yjs/Yrs ecosystem and enables universal way of
//! communicating between different systems.
//!
//! In its core y-sync protocol can operate as a simple state machine that serves exchanging and
//! responding to different message types described by the protocol. The [sync::DefaultProtocol]
//! provides all the message handlers necessary to make basic communication possible.
//!
//! y-sync protocol is extensible leaves a space for the users to define their own messages if
//! necessary:
//!
//! ```rust
//! use yrs::sync::{Awareness, Message, Protocol, Error};
//!
//! struct MyProtocol;
//! impl Protocol for MyProtocol {
//!     fn missing_handle(&self, awareness: &mut Awareness, tag: u8, data: Vec<u8>) -> Result<Option<Message>, Error> {
//!         // you can not only override existing message handlers but also define your own
//!         Ok(Some(Message::Custom(tag, data))) // echo
//!     }
//! }
//! ```
//!
//! # Querying document state
//!
//! Starting from version 0.23.0 Yrs provides a way to query document state using [JsonPath] queries.
//! These are available for transactions and [Any] types. They allow to extract specific parts of
//! the document state in a way similar to [JSONPath](https://en.wikipedia.org/wiki/JSONPath):
//!
//! ```rust
//! use yrs::{any, Delta, Doc, JsonPath, JsonPathEval, Out};
//!
//! let mut doc = Doc::new();
//! let mut txn = doc.transact_mut();
//!
//! // populate the document with some data to query
//! let friends = Delta::new()
//!     .insert(any!({ "name": "Bob", "nick": "boreas" }))
//!     .insert(any!({ "nick": "crocodile91" }));
//! let alice = Delta::new()
//!     .insert_attr("name", "Alice")
//!     .insert_attr("surname", "Smith")
//!     .insert_attr("age", 25)
//!     .insert_attr("friends", friends);
//! txn.node_mut("users").unwrap().insert(0, alice);
//!
//! let query = JsonPath::parse("$.users..friends.*.nick").unwrap();
//! let values: Vec<Out> = txn.json_path(&query).collect();
//! assert_eq!(values, vec![Out::Any(any!("boreas")), Out::Any(any!("crocodile91"))]);
//! ```
//!
//! # External learning materials
//!
//! - [A short walkthrough over YATA](https://bartoszsypytkowski.com/yata/) - a conflict resolution
//!   algorithm used by Yrs/Yjs.
//! - [Deep dive into internal architecture of Yrs](https://bartoszsypytkowski.com/yrs-architecture/).
//! - [Detailed explanation of conflict-free reordering algorithm](https://bartoszsypytkowski.com/yata-move/) used by Yrs.

mod alt;
pub mod block;
mod block_store;
pub mod doc;
mod event;
mod formatter;
mod id_map;
mod id_set;
mod transaction;
mod update;
mod utils;

pub mod any;
pub mod atomic;
pub mod cell;
mod cursor;
mod delta;
mod gc;
mod ids;
mod input;
pub mod iter;
pub mod json_path;
pub mod lib0;
mod meta;
pub mod node;
mod node_ref;
pub mod observer;
mod out;
mod slice;
mod state_vector;
mod sticky_index;
pub mod sync;
#[doc(hidden)]
pub mod test_utils;
mod transact;
pub mod undo;
#[cfg(feature = "weak")]
mod weak;

pub use crate::alt::{
    diff_updates_v1, diff_updates_v2, encode_state_vector_from_update_v1,
    encode_state_vector_from_update_v2, merge_updates_v1, merge_updates_v2,
};
pub use crate::any::Any;
pub use crate::block::ClientID;
use crate::block::ContentKind;
pub use crate::block::ID;
pub use crate::delta::{AttrOp, Attribute, Attribution, Delta, Op};
pub use crate::doc::Doc;
pub use crate::doc::OffsetKind;
pub use crate::doc::Options;
pub use crate::event::{
    Event, SubdocsEvent, SubdocsEventIter, TransactionCleanupEvent, UpdateEvent,
};
pub use crate::formatter::{
    AttrValue, AttributeValue, AttributedFragment, AttributedFragmentKind, AttributionsFormatter,
    DefaultFormatter, DeltaFormatter, DiffFormatter, RenderMode, SnapshotFormatter,
};
pub use crate::id_map::{AttrRange, ContentAttribute, ContentAttributes, Diff, IdMap};
pub use crate::id_set::IdSet;
pub use crate::input::In;
pub use crate::json_path::{JsonPath, JsonPathEval};
pub use crate::meta::{ContentIds, ContentMap};
pub use crate::node::NodeID;
pub use crate::node_ref::DeltaOptions;
pub use crate::node_ref::Formatted;
pub use crate::node_ref::Modified;
pub use crate::node_ref::NodeRef;
pub use crate::observer::{Observer, Subscription};
pub use crate::out::Out;
pub use crate::state_vector::Snapshot;
pub use crate::state_vector::StateVector;
pub use crate::sticky_index::Assoc;
pub use crate::sticky_index::IndexScope;
pub use crate::sticky_index::Offset;
pub use crate::sticky_index::StickyIndex;
pub use crate::transact::TransactionAcqError;
pub use crate::transaction::DocHandle;
pub use crate::transaction::Origin;
pub use crate::transaction::RootRefs;
pub use crate::transaction::Transaction;
pub use crate::transaction::TransactionMut;
pub use crate::transaction::TransactionState;
pub use crate::update::Update;
#[cfg(feature = "weak")]
pub use crate::weak::{LinkSource, Quotable, QuoteError, Unquote};

pub use crate::cell::Acquire;
pub use crate::cell::AcquireMut;

#[cfg(not(feature = "sync"))]
pub type Cell<T> = crate::cell::RcCell<T>;
#[cfg(not(feature = "sync"))]
pub type OwnedRef<T> = crate::cell::OwnedRef<T>;
#[cfg(not(feature = "sync"))]
pub type OwnedMut<T> = crate::cell::OwnedMut<T>;

#[cfg(feature = "sync")]
pub type Cell<T> = crate::cell::sync::ArcCell<T>;
#[cfg(feature = "sync")]
pub type OwnedRef<T> = crate::cell::sync::OwnedRef<T>;
#[cfg(feature = "sync")]
pub type OwnedMut<T> = crate::cell::sync::OwnedMut<T>;

#[deprecated(since = "0.19.0", note = "Use `yrs::Out` instead")]
pub type Value = Out;

pub type UndoManager = crate::undo::UndoManager<()>;
pub type Uuid = std::sync::Arc<str>;

/// Generate random v4 UUID.
/// (See: https://www.rfc-editor.org/rfc/rfc4122#section-4.4)
pub fn uuid_v4() -> Uuid {
    let mut rand = fastrand::Rng::new();
    uuid_v4_from(rand.u128(..))
}

/// Generate random v4 UUID.
/// (See: https://www.rfc-editor.org/rfc/rfc4122#section-4.4)
pub fn uuid_v4_from(value: u128) -> Uuid {
    let uuid = value & 0xFFFFFFFFFFFF4FFFBFFFFFFFFFFFFFFF | 0x40008000000000000000;
    let src = uuid.to_be_bytes();
    const HEX: [u8; 16] = [
        b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'a', b'b', b'c', b'd', b'e',
        b'f',
    ];
    let mut dst = [0; 36];
    dst[0] = HEX[(src[0] >> 4) as usize];
    dst[1] = HEX[(src[0] & 0x0f) as usize];
    dst[2] = HEX[(src[1] >> 4) as usize];
    dst[3] = HEX[(src[1] & 0x0f) as usize];
    dst[4] = HEX[(src[2] >> 4) as usize];
    dst[5] = HEX[(src[2] & 0x0f) as usize];
    dst[6] = HEX[(src[3] >> 4) as usize];
    dst[7] = HEX[(src[3] & 0x0f) as usize];
    dst[8] = '-' as u8;
    dst[9] = HEX[(src[4] >> 4) as usize];
    dst[10] = HEX[(src[4] & 0x0f) as usize];
    dst[11] = HEX[(src[5] >> 4) as usize];
    dst[12] = HEX[(src[5] & 0x0f) as usize];
    dst[13] = '-' as u8;
    dst[14] = HEX[(src[6] >> 4) as usize];
    dst[15] = HEX[(src[6] & 0x0f) as usize];
    dst[16] = HEX[(src[7] >> 4) as usize];
    dst[17] = HEX[(src[7] & 0x0f) as usize];
    dst[18] = '-' as u8;
    dst[19] = HEX[(src[8] >> 4) as usize];
    dst[20] = HEX[(src[8] & 0x0f) as usize];
    dst[21] = HEX[(src[9] >> 4) as usize];
    dst[22] = HEX[(src[9] & 0x0f) as usize];
    dst[23] = '-' as u8;
    dst[24] = HEX[(src[10] >> 4) as usize];
    dst[25] = HEX[(src[10] & 0x0f) as usize];
    dst[26] = HEX[(src[11] >> 4) as usize];
    dst[27] = HEX[(src[11] & 0x0f) as usize];
    dst[28] = HEX[(src[12] >> 4) as usize];
    dst[29] = HEX[(src[12] & 0x0f) as usize];
    dst[30] = HEX[(src[13] >> 4) as usize];
    dst[31] = HEX[(src[13] & 0x0f) as usize];
    dst[32] = HEX[(src[14] >> 4) as usize];
    dst[33] = HEX[(src[14] & 0x0f) as usize];
    dst[34] = HEX[(src[15] >> 4) as usize];
    dst[35] = HEX[(src[15] & 0x0f) as usize];
    let str: &str = unsafe { std::str::from_utf8_unchecked(&dst) };
    str.into()
}

pub type Result<T, E = Box<Error>> = std::result::Result<T, E>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Lib0(#[from] lib0::Error),
    #[error("GC needs to be turned off")]
    Gc,
    #[error("invalid parent: block {parent} should be of type node, but was {kind:?}")]
    InvalidParent { parent: ID, kind: ContentKind },
}
