use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use iris_mpc_common::shamir::P;
use iris_mpc_gpu::{
    dot::share_db::{preprocess_query, ShareDB},
    helpers::device_manager::DeviceManager,
};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::sync::Arc;

fn random_vec(n: usize, m: usize, max_value: u32) -> Vec<u16> {
    let mut rng = StdRng::seed_from_u64(RNG_SEED);
    (0..n * m)
        .map(|_| rng.gen_range(0..max_value) as u16)
        .collect()
}

const RNG_SEED: u64 = 42;
const DB_SIZE: usize = 8 * 300_000;
const QUERY_SIZE: usize = 930;
const WIDTH: usize = 12800;

fn bench_memcpy(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_memcpy");

    let db = random_vec(DB_SIZE, WIDTH, P as u32);
    let query = random_vec(QUERY_SIZE, WIDTH, P as u32);
    let device_manager = Arc::new(DeviceManager::init());

    let mut engine = ShareDB::init(
        0,
        device_manager.clone(),
        DB_SIZE,
        QUERY_SIZE,
        ([0u32; 8], [0u32; 8]),
        vec![],
    );
    let preprocessed_query = preprocess_query(&query);
    let streams = device_manager.fork_streams();
    let blass = device_manager.create_cublas(&streams);
    let (db_slices, db_sizes) = engine.load_db(&db, DB_SIZE, DB_SIZE, false);

    group.throughput(Throughput::Elements((DB_SIZE * QUERY_SIZE / 31) as u64));
    group.sample_size(10);

    group.bench_function(format!("matmul {} x {}", DB_SIZE, QUERY_SIZE), |b| {
        b.iter(|| {
            let preprocessed_query = device_manager
                .htod_transfer_query(&preprocessed_query, &streams, QUERY_SIZE)
                .unwrap();
            let query_sums = engine.query_sums(&preprocessed_query, &streams, &blass);
            engine.dot(
                &preprocessed_query,
                &db_slices.code_gr,
                &db_sizes,
                0,
                &streams,
                &blass,
            );
            engine.dot_reduce(&query_sums, &db_slices.code_sums_gr, &db_sizes, 0, &streams);
            device_manager.await_streams(&streams);
        });
    });
}

criterion_group!(benches, bench_memcpy);
criterion_main!(benches);
