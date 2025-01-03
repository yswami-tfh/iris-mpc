#[cfg(feature = "gpu_dependent")]
mod threshold_test {
    use cudarc::driver::{CudaDevice, CudaStream};
    use iris_mpc_common::iris_db::iris::{IrisCodeArray, MATCH_THRESHOLD_RATIO};
    use iris_mpc_gpu::{
        helpers::{device_manager::DeviceManager, dtoh_on_stream_sync, htod_on_stream_sync},
        threshold_ring::protocol::{ChunkShare, Circuits},
    };
    use itertools::izip;
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use static_assertions::const_assert;
    use std::{env, sync::Arc};
    use tokio::time::Instant;

    // ceil(930 * 125_000 / 2048) * 2048
    // const INPUTS_PER_GPU_SIZE: usize = 116_250_624;
    const INPUTS_PER_GPU_SIZE: usize = 12_507_136;
    const CHUNK_SIZE: usize = INPUTS_PER_GPU_SIZE / 64;

    const B_BITS: u64 = 16;
    pub(crate) const B: u64 = 1 << B_BITS;
    pub(crate) const A: u64 = ((1. - 2. * MATCH_THRESHOLD_RATIO) * B as f64) as u64;

    fn sample_code_dots<R: Rng>(size: usize, rng: &mut R) -> Vec<u16> {
        (0..size)
            .map(|_| {
                let mut x = rng.gen_range::<u16, _>(0..=IrisCodeArray::IRIS_CODE_SIZE as u16);
                let neg = rng.gen::<bool>();
                if neg {
                    x = u16::MAX - x + 1;
                }
                x
            })
            .collect::<Vec<_>>()
    }

    fn sample_mask_dots<R: Rng>(size: usize, rng: &mut R) -> Vec<u16> {
        (0..size)
            .map(|_| rng.gen_range::<u16, _>(0..=IrisCodeArray::IRIS_CODE_SIZE as u16))
            .collect::<Vec<_>>()
    }

    fn rep_share<R: Rng>(value: u16, id: usize, rng: &mut R) -> (u16, u16) {
        let a = rng.gen();
        let b = rng.gen();
        let c = value - a - b;

        match id {
            0 => (a, c),
            1 => (b, a),
            2 => (c, b),
            _ => unreachable!(),
        }
    }

    fn rep_share_vec<R: Rng>(value: &[u16], id: usize, rng: &mut R) -> (Vec<u16>, Vec<u16>) {
        let mut a = Vec::with_capacity(value.len());
        let mut b = Vec::with_capacity(value.len());
        for v in value.iter() {
            let (a_, b_) = rep_share(*v, id, rng);
            a.push(a_);
            b.push(b_);
        }
        (a, b)
    }

    fn to_gpu(
        a: &[u16],
        b: &[u16],
        devices: &[Arc<CudaDevice>],
        streams: &[CudaStream],
    ) -> Vec<ChunkShare<u16>> {
        debug_assert_eq!(a.len(), b.len());

        let mut result = Vec::with_capacity(devices.len());

        for (dev, stream, a, b) in izip!(
            devices,
            streams,
            a.chunks(INPUTS_PER_GPU_SIZE),
            b.chunks(INPUTS_PER_GPU_SIZE)
        ) {
            let a_ = htod_on_stream_sync(a, dev, stream).unwrap();
            let b_ = htod_on_stream_sync(b, dev, stream).unwrap();
            result.push(ChunkShare::new(a_, b_));
        }

        result
    }

    fn pack_with_device_padding(bits: Vec<bool>) -> Vec<u64> {
        assert!(bits.len() % INPUTS_PER_GPU_SIZE == 0);
        let mut res = vec![];
        for devices in bits.chunks_exact(INPUTS_PER_GPU_SIZE) {
            for bits in devices.chunks(64) {
                let mut r = 0;
                for (i, bit) in bits.iter().enumerate() {
                    r |= u64::from(*bit) << i;
                }
                res.push(r);
            }
        }
        res
    }

    fn real_result_msb(code_input: Vec<u16>, mask_input: Vec<u16>) -> Vec<u64> {
        assert_eq!(code_input.len(), mask_input.len());
        let mod_ = 1u64 << (16 + B_BITS);
        let mut res = Vec::with_capacity(code_input.len());
        for (c, m) in code_input.into_iter().zip(mask_input) {
            let r = ((m as u64) * A - ((c as u64) << B_BITS)) % mod_;
            let msb = r >> (B_BITS + 16 - 1) & 1 == 1;
            res.push(msb)
        }
        pack_with_device_padding(res)
    }

    fn open(party: &mut Circuits, x: &[ChunkShare<u64>], streams: &[CudaStream]) -> Vec<u64> {
        let n_devices = x.len();
        let mut a = Vec::with_capacity(n_devices);
        let mut b = Vec::with_capacity(n_devices);
        let mut c = Vec::with_capacity(n_devices);

        cudarc::nccl::result::group_start().unwrap();
        for (idx, res) in x.iter().enumerate() {
            // Result is in bit 0
            let res = res.get_offset(0, CHUNK_SIZE);
            party.comms()[idx]
                .send_view(&res.b, party.next_id(), &streams[idx])
                .unwrap();
            a.push(res.a);
            b.push(res.b);
        }
        for (idx, res) in x.iter().enumerate() {
            let mut res = res.get_offset(1, CHUNK_SIZE);
            party.comms()[idx]
                .receive_view(&mut res.a, party.prev_id(), &streams[idx])
                .unwrap();
            c.push(res.a);
        }
        cudarc::nccl::result::group_end().unwrap();

        let mut result = Vec::with_capacity(n_devices * CHUNK_SIZE);
        let devices = party.get_devices();
        for (dev, stream, a, b, c) in izip!(devices, streams, a, b, c) {
            let mut a = dtoh_on_stream_sync(&a, &dev, stream).unwrap();
            let b = dtoh_on_stream_sync(&b, &dev, stream).unwrap();
            let c = dtoh_on_stream_sync(&c, &dev, stream).unwrap();
            for (a, b, c) in izip!(a.iter_mut(), b, c) {
                *a ^= b ^ c;
            }
            result.extend(a);
        }
        assert_eq!(result.len(), n_devices * CHUNK_SIZE);
        result
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn test_threshold() -> eyre::Result<()> {
        use itertools::Itertools;

        const_assert!(
            INPUTS_PER_GPU_SIZE % (2048) == 0,
            // Mod 16 for randomness, mod 64 for chunk size
        );
        // TODO
        let mut rng = StdRng::seed_from_u64(42);

        let party_id: usize = env::var("SMPC__PARTY_ID")
            .expect("SMPC__PARTY_ID environment variable not set")
            .parse()
            .expect("SMPC__PARTY_ID must be a valid usize");
        let n_devices = CudaDevice::count()? as usize;

        // Get inputs
        let code_dots = sample_code_dots(INPUTS_PER_GPU_SIZE * n_devices, &mut rng);
        let mask_dots = sample_mask_dots(INPUTS_PER_GPU_SIZE * n_devices, &mut rng);

        let (code_share_a, code_share_b) = rep_share_vec(&code_dots, party_id, &mut rng);
        let (mask_share_a, mask_share_b) = rep_share_vec(&mask_dots, party_id, &mut rng);
        let real_result = real_result_msb(code_dots, mask_dots);
        println!("Random shared inputs generated!");

        // Get Circuit Party
        let device_manager = Arc::new(DeviceManager::init());
        let ids = device_manager.get_ids_from_magic(0);
        let comms = device_manager.instantiate_network_from_ids(party_id, &ids)?;
        let mut party = Circuits::new(
            party_id,
            INPUTS_PER_GPU_SIZE,
            INPUTS_PER_GPU_SIZE / 64,
            ([party_id as u32; 8], [((party_id + 2) % 3) as u32; 8]),
            device_manager.clone(),
            comms,
        );
        let devices = party.get_devices();
        let streams = devices
            .iter()
            .map(|dev| dev.fork_default_stream().unwrap())
            .collect::<Vec<_>>();

        // Import to GPU
        let code_gpu = to_gpu(&code_share_a, &code_share_b, &devices, &streams);
        let mask_gpu = to_gpu(&mask_share_a, &mask_share_b, &devices, &streams);
        println!("Data is on GPUs!");
        println!("Starting tests...");

        for _ in 0..10 {
            let code_gpu = code_gpu.iter().map(|x| x.as_view()).collect_vec();
            let mask_gpu = mask_gpu.iter().map(|x| x.as_view()).collect_vec();

            let now = Instant::now();
            party.compare_threshold_masked_many(&code_gpu, &mask_gpu, &streams);
            party.synchronize_streams(&streams);
            println!("compute time: {:?}", now.elapsed());

            let res = party.take_result_buffer();
            let now = Instant::now();
            let result = open(&mut party, &res, &streams);
            party.synchronize_streams(&streams);
            party.return_result_buffer(res);
            println!("Open and transfer to CPU time: {:?}", now.elapsed());

            let mut correct = true;
            for (i, (r, r_)) in izip!(&result, &real_result).enumerate() {
                if r != r_ {
                    correct = false;
                    println!("Test failed on index: {}: {} != {}", i, r, r_);
                    break;
                }
            }
            if correct {
                println!("Test passed!");
            }
        }

        Ok(())
    }
}
