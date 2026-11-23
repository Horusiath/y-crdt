use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

use crate::block::ClientID;
use crate::lib0::{Decode, Decoder, Encode, Encoder};

/// Timestamp used by [crate::sync::Awareness] to tag most recent updates.
pub type Timestamp = u64;

pub type AwarenessMap<T = crate::Any> = HashMap<ClientID, ClientState<T>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientState<T> {
    pub clock: u32,
    pub last_updated: Timestamp,
    pub data: Option<T>,
}

impl<T> ClientState<T> {
    fn new(clock: u32, last_updated: Timestamp, data: Option<T>) -> Self {
        ClientState {
            clock,
            last_updated,
            data,
        }
    }
}

impl<T> Default for ClientState<T> {
    #[inline]
    fn default() -> Self {
        ClientState {
            clock: 0,
            last_updated: 0,
            data: None,
        }
    }
}

/// The Awareness trait implements a simple shared state protocol that can be used for non-persistent
/// data like awareness information (cursor, username, status, ..). Each client can update its own
/// local state and listen to state changes of remote clients.
///
/// Each client is identified by a unique client id (something we borrow from `doc.clientID`).
/// A client can override its own state by propagating a message with an increasing timestamp
/// (`clock`). If such a message is received, it is applied if the known state of that client is
/// older than the new state (`clock < new_clock`). If a client thinks that a remote client is
/// offline, it may propagate a message with `{ clock, state: null, client }`. If such a message is
/// received, and the known clock of that client equals the received clock, it will clean the state.
///
/// Before a client disconnects, it should propagate a `null` state with an updated clock.
pub trait Awareness {
    type State: Serialize + DeserializeOwned + PartialEq;

    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    fn now(&self) -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as Timestamp
    }

    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    fn now(&self) -> Timestamp {
        unimplemented!()
    }

    /// Returns a state map of all the clients tracked by current [Awareness] instance. Those
    /// states are identified by their corresponding [ClientID]s. The associated state is
    /// represented and replicated to other clients as a JSON string.
    fn clients(&self) -> impl Iterator<Item = (&ClientID, &ClientState<Self::State>)>;

    /// Gets the state of a particular client.
    fn state(&self, client_id: ClientID) -> Option<&ClientState<Self::State>>;

    /// Gets a mutable reference to the state of a particular client, initializing it with default
    /// values if necessary.
    fn state_or_default(&mut self, client_id: ClientID) -> &mut ClientState<Self::State>;

    /// Sets a current [Awareness] instance state to a corresponding JSON string. This state will
    /// be replicated to other clients as part of the [AwarenessUpdate]. The clock is bumped on
    /// every call. Returns the previous state if the new one differs from it, or `None` if there
    /// was no previous state or the state didn't change.
    fn insert_state(&mut self, client_id: ClientID, data: Self::State) -> Option<Self::State> {
        let timestamp = self.now();
        let state = self.state_or_default(client_id);
        state.clock += 1;
        state.last_updated = timestamp;
        if state.data.as_ref() != Some(&data) {
            state.data.replace(data)
        } else {
            None
        }
    }

    /// Clears out a state of a given client, effectively marking it as disconnected.
    /// Returns the removed state, or `None` if that client had no state.
    fn remove_state(&mut self, client_id: ClientID) -> Option<Self::State> {
        let timestamp = self.now();
        let state = self.state_or_default(client_id);
        if state.last_updated == 0 {
            state.last_updated = timestamp;
        }
        state.clock += 1;
        state.data.take()
    }

    /// Returns a serializable update object which is representation of a current Awareness state.
    /// This doesn't include states that have been removed in the past.
    fn update(&self) -> Result<AwarenessUpdate, Error> {
        let clients: Vec<_> = self
            .clients()
            .flat_map(|(key, e)| if e.data.is_none() { None } else { Some(*key) })
            .collect();
        self.update_with_clients(clients)
    }

    /// Returns a serializable update object which is representation of a current Awareness state.
    /// Unlike [Awareness::update], this method variant allows to prepare update only for a subset
    /// of known clients. These clients must all be known to a current [Awareness] instance,
    /// otherwise a [Error::ClientNotFound] error will be returned.
    fn update_with_clients<I: IntoIterator<Item = ClientID>>(
        &self,
        clients: I,
    ) -> Result<AwarenessUpdate, Error> {
        let mut res = HashMap::new();
        for client_id in clients {
            let (clock, json) = if let Some(meta) = self.state(client_id) {
                let data = serde_json::to_string(&meta.data)?;
                (meta.clock, data)
            } else {
                return Err(Error::ClientNotFound(client_id));
            };
            res.insert(client_id, AwarenessUpdateEntry { clock, json });
        }
        Ok(AwarenessUpdate { clients: res })
    }

    /// Applies an update (incoming from remote channel or generated using [Awareness::update] /
    /// [Awareness::update_with_clients] methods) and modifies a state of a current instance.
    /// Returns an [AwarenessUpdateSummary] describing the clients affected by this call.
    fn apply_update(
        &mut self,
        local_id: ClientID,
        update: AwarenessUpdate,
    ) -> Result<AwarenessUpdateSummary, Error> {
        let now = self.now();
        let mut summary = AwarenessUpdateSummary::default();

        for (client_id, entry) in update.clients {
            let mut clock = entry.clock;
            let new: Option<Self::State> = serde_json::from_str(&entry.json)?;
            let known = self.state(client_id).is_some();
            let state = self.state_or_default(client_id);
            let is_removed = state.clock == clock && new.is_none() && state.data.is_some();
            if state.clock < clock || is_removed {
                match new {
                    None => {
                        // never let a remote client remove this local state
                        if client_id == local_id && state.data.is_some() {
                            // remote client removed the local state. Do not remove state. Broadcast a message indicating
                            // that this client still exists by increasing the clock
                            clock += 1;
                        } else {
                            state.data = None;
                            if known {
                                summary.removed.push(client_id);
                            }
                        }
                    }
                    new => {
                        if !known {
                            summary.added.push(client_id);
                        } else {
                            summary.updated.push(client_id);
                            if state.data.is_some() && state.data != new {
                                summary.changed.push(client_id);
                            }
                        }
                        state.data = new;
                    }
                }
                state.clock = clock;
                state.last_updated = now;
            }
        }

        Ok(summary)
    }
}

impl<T> Awareness for HashMap<ClientID, ClientState<T>>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    type State = T;

    fn clients(&self) -> impl Iterator<Item = (&ClientID, &ClientState<T>)> {
        self.iter()
    }

    fn state(&self, client_id: ClientID) -> Option<&ClientState<T>> {
        self.get(&client_id)
    }

    fn state_or_default(&mut self, client_id: ClientID) -> &mut ClientState<T> {
        self.entry(client_id).or_default()
    }
}

/// Summary of applying an [AwarenessUpdate] over [Awareness::apply_update] method.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AwarenessUpdateSummary {
    /// New clients added as part of the update.
    pub added: Vec<ClientID>,
    /// Existing clients that have been updated by the update, whether or not their state changed.
    pub updated: Vec<ClientID>,
    /// Subset of [AwarenessUpdateSummary::updated] whose state actually changed.
    pub changed: Vec<ClientID>,
    /// Existing clients that have been removed by the update.
    pub removed: Vec<ClientID>,
}

impl AwarenessUpdateSummary {
    /// Returns a collection of all clients that have been changed by the [AwarenessUpdate].
    pub fn all_changes(&self) -> Vec<ClientID> {
        let mut res =
            Vec::with_capacity(self.added.len() + self.updated.len() + self.removed.len());
        res.extend_from_slice(&self.added);
        res.extend_from_slice(&self.updated);
        res.extend_from_slice(&self.removed);
        res
    }

    /// Returns `true` if the update didn't affect any client.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

/// A structure that represents an encodable state of an [Awareness] struct.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AwarenessUpdate {
    /// Client updates.
    pub clients: HashMap<ClientID, AwarenessUpdateEntry>,
}

impl Encode for AwarenessUpdate {
    fn encode<E: Encoder>(&self, encoder: &mut E) {
        encoder.write_var(self.clients.len());
        for (&client_id, e) in self.clients.iter() {
            encoder.write_var(client_id.get());
            encoder.write_var(e.clock);
            encoder.write_string(&e.json);
        }
    }
}

impl Decode for AwarenessUpdate {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, crate::lib0::Error> {
        let len: usize = decoder.read_var()?;
        let mut clients = HashMap::new();
        clients.try_reserve(len)?;
        for _ in 0..len {
            let client_id = ClientID::new(decoder.read_var::<u64>()?);
            let clock: u32 = decoder.read_var()?;
            let json: String = decoder.read_string()?.into();
            clients.insert(client_id, AwarenessUpdateEntry { clock, json });
        }

        Ok(AwarenessUpdate { clients })
    }
}

/// A single client entry of an [AwarenessUpdate]. It consists of logical clock and JSON client
/// state represented as a string.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AwarenessUpdateEntry {
    /// Timestamp used to recognize which update is the latest one.
    pub clock: u32,
    /// String with JSON payload containing user data.
    pub json: String,
}

/// Errors generated by an [Awareness] struct methods.
#[derive(Error, Debug)]
pub enum Error {
    /// Client ID was not found in [Awareness] metadata.
    #[error("client ID `{0}` not found")]
    ClientNotFound(ClientID),
    #[error("couldn't serialize awareness state: {0}")]
    Serde(#[from] serde_json::Error),
}

#[cfg(test)]
mod test {
    use crate::block::ClientID;
    use crate::lib0::Decode;
    use crate::sync::awareness::{AwarenessMap, AwarenessUpdateSummary};
    use crate::sync::{Awareness, AwarenessUpdate};
    use crate::{Any, any};
    use std::collections::HashMap;

    #[test]
    fn decode_rejects_length_amplification() {
        // Regression: an awareness update arrives over the network; a length prefix declaring a
        // huge client count (a few bytes) must not trigger an eager multi-hundred-MB allocation.
        // `try_reserve` turns it into a recoverable decode error instead of an abort under a hard
        // memory limit. Bytes: var-int len ~250M (0xFF 0xFF 0xFF 0x7A) + no data.
        let adversarial = [0xFFu8, 0xFF, 0xFF, 0x7A];
        assert!(
            AwarenessUpdate::decode_v1(&adversarial).is_err(),
            "an oversized awareness client count must be rejected, not eagerly allocated"
        );
    }

    #[test]
    fn awareness() {
        const LOCAL_ID: ClientID = ClientID::new(1);
        const REMOTE_ID: ClientID = ClientID::new(2);

        let mut local = AwarenessMap::<Any>::new();
        let mut remote = AwarenessMap::<Any>::new();

        let prev = local.insert_state(LOCAL_ID, any!({"x":3}));
        assert_eq!(prev, None);
        let summary = remote
            .apply_update(REMOTE_ID, local.update().unwrap())
            .unwrap();
        assert_eq!(
            remote.state(ClientID::new(1)).unwrap().data,
            Some(any!({"x":3}))
        );
        assert_eq!(remote.state(ClientID::new(1)).unwrap().clock, 1);
        assert_eq!(summary.added, &[ClientID::new(1)]);

        let prev = local.insert_state(LOCAL_ID, any!({"x":4}));
        assert_eq!(prev, Some(any!({"x":3})));
        let summary = remote
            .apply_update(REMOTE_ID, local.update().unwrap())
            .unwrap();
        assert_eq!(
            remote.state(ClientID::new(1)).unwrap().data,
            Some(any!({"x":4}))
        );
        assert_eq!(
            summary,
            AwarenessUpdateSummary {
                added: vec![],
                updated: vec![ClientID::new(1)],
                changed: vec![ClientID::new(1)],
                removed: vec![]
            }
        );

        // same state: clock bumps, remote sees an update but no change
        assert_eq!(local.insert_state(LOCAL_ID, any!({"x":4})), None);
        let summary = remote
            .apply_update(REMOTE_ID, local.update().unwrap())
            .unwrap();
        assert_eq!(remote.state(ClientID::new(1)).unwrap().clock, 3);
        assert_eq!(summary.updated, &[ClientID::new(1)]);
        assert!(summary.changed.is_empty());

        assert_eq!(local.remove_state(LOCAL_ID), Some(any!({"x":4})));
        assert_eq!(local.remove_state(LOCAL_ID), None);
        let summary = remote
            .apply_update(
                REMOTE_ID,
                local.update_with_clients([ClientID::new(1)]).unwrap(),
            )
            .unwrap();
        assert_eq!(summary.removed, &[ClientID::new(1)]);
        assert!(local.state(ClientID::new(1)).unwrap().data.is_none());
        assert!(remote.state(ClientID::new(1)).unwrap().data.is_none());

        // stale update is ignored
        let summary = remote
            .apply_update(
                REMOTE_ID,
                local.update_with_clients([ClientID::new(1)]).unwrap(),
            )
            .unwrap();
        assert!(summary.is_empty());
    }

    /// Replicated part of each client state; `last_updated` is local to every replica.
    fn replicated(states: &AwarenessMap<Any>) -> HashMap<ClientID, (u32, Option<Any>)> {
        states
            .iter()
            .map(|(id, s)| (*id, (s.clock, s.data.clone())))
            .collect()
    }

    #[test]
    fn awareness_summary() -> Result<(), Box<dyn std::error::Error>> {
        const LOCAL_ID: ClientID = ClientID::new(1);
        const REMOTE_ID: ClientID = ClientID::new(2);

        let mut local = AwarenessMap::<Any>::new();
        let mut remote = AwarenessMap::<Any>::new();

        assert_eq!(local.insert_state(LOCAL_ID, any!({"x":3})), None);
        let update = local.update_with_clients([LOCAL_ID])?;
        let summary = remote.apply_update(REMOTE_ID, update)?;
        assert_eq!(
            remote.state(ClientID::new(1)).unwrap().data,
            Some(any!({"x":3}))
        );
        assert_eq!(remote.state(ClientID::new(1)).unwrap().clock, 1);
        assert_eq!(
            summary,
            AwarenessUpdateSummary {
                added: vec![ClientID::new(1)],
                updated: vec![],
                changed: vec![],
                removed: vec![]
            }
        );

        local.insert_state(LOCAL_ID, any!({"x":4})).unwrap();
        let update = local.update_with_clients([LOCAL_ID])?;
        let summary = remote.apply_update(REMOTE_ID, update)?;
        assert_eq!(
            remote.state(ClientID::new(1)).unwrap().data,
            Some(any!({"x":4}))
        );
        assert_eq!(
            summary,
            AwarenessUpdateSummary {
                added: vec![],
                updated: vec![ClientID::new(1)],
                changed: vec![ClientID::new(1)],
                removed: vec![]
            }
        );
        assert_eq!(replicated(&local), replicated(&remote));

        local.remove_state(LOCAL_ID);
        let update = local.update_with_clients([LOCAL_ID])?;
        let summary = remote.apply_update(REMOTE_ID, update)?;
        assert_eq!(
            summary,
            AwarenessUpdateSummary {
                added: vec![],
                updated: vec![],
                changed: vec![],
                removed: vec![ClientID::new(1)]
            }
        );
        assert!(local.state(ClientID::new(1)).unwrap().data.is_none());
        assert_eq!(replicated(&local), replicated(&remote));
        Ok(())
    }
}
