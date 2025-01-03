//! Exchange the SyncState between parties using NCCL.

use crate::helpers::comm::NcclComm;
use cudarc::driver::DeviceSlice;
use eyre::{eyre, Result};
use iris_mpc_common::helpers::sync::{SyncResult, SyncState};

pub fn sync(comm: &NcclComm, state: &SyncState) -> Result<SyncResult> {
    let state_dev = comm.device().htod_copy(serialize(state)?).unwrap();
    let mut all_states_dev = comm
        .device()
        .alloc_zeros(state_dev.len() * comm.world_size())
        .unwrap();

    comm.all_gather(&state_dev, &mut all_states_dev)
        .map_err(|e| eyre!("{:?}", e.0))?;

    let all_states_ser = comm.device().dtoh_sync_copy(&all_states_dev).unwrap();
    let all_states = deserialize_all(&all_states_ser)?;
    Ok(SyncResult::new(state.clone(), all_states))
}

// Change these parameters together - see unittests below.
/// The fixed serialization size of SyncState.
pub const MAX_REQUESTS: usize = 256 * 2;
const MAX_REQUEST_ID_LEN: usize = 36; // uuidv4 string
const SERIAL_SIZE: usize =
    MAX_REQUESTS * (size_of::<usize>() + MAX_REQUEST_ID_LEN) + 2 * size_of::<usize>();

/// Serialize the state to a fixed-size buffer suitable for all_gather.
fn serialize(state: &SyncState) -> Result<Vec<u8>> {
    let mut state_ser = bincode::serialize(state)?;
    if state_ser.len() > SERIAL_SIZE {
        return Err(eyre!("State too large to serialize"));
    }
    state_ser.extend(std::iter::repeat(0).take(SERIAL_SIZE - state_ser.len()));
    Ok(state_ser)
}

/// Deserialize the state from a fixed-size buffer.
fn deserialize(state_ser: &[u8]) -> Result<SyncState> {
    Ok(bincode::deserialize(state_ser)?)
}

/// Deserialize all states concatenated in a buffer (the output of all_gather).
fn deserialize_all(state_ser: &[u8]) -> Result<Vec<SyncState>> {
    state_ser.chunks(SERIAL_SIZE).map(deserialize).collect()
}

#[cfg(test)]
#[cfg(feature = "gpu_dependent")]
mod tests {
    use super::*;
    use cudarc::{driver::CudaDevice, nccl::Id};
    use eyre::Result;
    use tokio::task::JoinSet;

    #[test]
    fn test_serialize() -> Result<()> {
        // My state.
        let state = SyncState {
            db_len:              123,
            deleted_request_ids: vec!["A".repeat(MAX_REQUEST_ID_LEN); MAX_REQUESTS],
        };
        let state_ser = serialize(&state)?;
        assert_eq!(state_ser.len(), SERIAL_SIZE);
        // Concatenation of states from 3 parties.
        let all_states_ser = vec![state_ser.clone(); 3].concat();
        let all_states = deserialize_all(&all_states_ser)?;

        for s in all_states.iter() {
            assert_eq!(s, &state);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_sync() -> Result<()> {
        let n_parties = 3.min(CudaDevice::count()? as usize);
        let net_id = Id::new().unwrap();
        let expected_state = some_state();

        let sync_task = |i| {
            let my_state = expected_state.clone();
            move || {
                let device = CudaDevice::new(i).unwrap();
                let comm = NcclComm::from_rank(device, i, n_parties, net_id).unwrap();
                sync(&comm, &my_state).unwrap()
            }
        };

        let mut tasks = JoinSet::new();
        for i in 0..n_parties {
            tasks.spawn_blocking(sync_task(i));
        }

        while let Some(result) = tasks.join_next().await {
            assert_eq!(result?.must_rollback_storage(), None);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_out_of_sync() -> Result<()> {
        let n_parties = 3.min(CudaDevice::count()? as usize);
        let net_id = Id::new().unwrap();

        let sync_task = |i| {
            let my_state = if i == 0 {
                some_state()
            } else {
                SyncState {
                    db_len:              12, // late
                    deleted_request_ids: vec![],
                }
            };
            move || {
                let device = CudaDevice::new(i).unwrap();
                let comm = NcclComm::from_rank(device, i, n_parties, net_id).unwrap();
                sync(&comm, &my_state).unwrap()
            }
        };

        let mut tasks = JoinSet::new();
        for i in 0..n_parties {
            tasks.spawn_blocking(sync_task(i));
        }

        while let Some(result) = tasks.join_next().await {
            assert_eq!(result?.must_rollback_storage(), Some(12));
        }
        Ok(())
    }

    fn some_state() -> SyncState {
        SyncState {
            db_len:              123,
            deleted_request_ids: vec!["abc".to_string(), "def".to_string()],
        }
    }
}
