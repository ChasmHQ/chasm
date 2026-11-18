use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct MutationGate {
    inner: Arc<Mutex<()>>,
}

impl MutationGate {
    pub async fn acquire(&self, cancel: &CancellationToken) -> Option<MutationLease> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            lease = self.inner.clone().lock_owned() => Some(MutationLease(lease)),
        }
    }

    pub fn try_acquire(&self) -> Option<MutationLease> {
        self.inner.clone().try_lock_owned().ok().map(MutationLease)
    }

    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.inner.try_lock().is_err()
    }
}

/// Holding the lease holds the gate; dropping it releases the gate.
pub struct MutationLease(#[allow(dead_code)] OwnedMutexGuard<()>);

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_gate_serializes_every_mutation_owner() {
        let gate = MutationGate::default();
        let lease = gate.try_acquire().unwrap();
        assert!(gate.busy());
        assert!(gate.try_acquire().is_none());
        drop(lease);
        assert!(!gate.busy());
        assert!(gate.try_acquire().is_some());
    }

    #[tokio::test]
    async fn cancelled_wait_does_not_acquire_later() {
        let gate = MutationGate::default();
        let _lease = gate.try_acquire().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(gate.acquire(&cancel).await.is_none());
    }
}
