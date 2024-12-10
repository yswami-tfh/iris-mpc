use super::{graph_store::PyGraphStore, iris_code::PyIrisCode, plaintext_store::PyPlaintextStore};
use hawk_pack::{
    hawk_searcher::{HawkerParams, N_PARAM_LAYERS},
    HawkSearcher,
};
use iris_mpc_cpu::py_bindings;
use pyo3::{exceptions::PyIOError, prelude::*};

#[pyclass]
#[derive(Clone, Default)]
pub struct PyHawkSearcher(pub HawkSearcher);

#[pymethods]
#[allow(non_snake_case)]
impl PyHawkSearcher {
    #[new]
    pub fn new(M: usize, ef_constr: usize, ef_search: usize) -> Self {
        Self::new_standard(ef_constr, ef_search, M)
    }

    #[staticmethod]
    pub fn new_standard(M: usize, ef_constr: usize, ef_search: usize) -> Self {
        let params = HawkerParams::new(ef_constr, ef_search, M);
        Self(HawkSearcher { params })
    }

    #[staticmethod]
    pub fn new_uniform(M: usize, ef: usize) -> Self {
        let params = HawkerParams::new_uniform(ef, M);
        Self(HawkSearcher { params })
    }

    /// Construct `HawkSearcher` with fully general parameters, specifying the
    /// values of various parameters used during construction and search at
    /// different levels of the graph hierarchy.
    #[staticmethod]
    pub fn new_general(
        M: [usize; N_PARAM_LAYERS],
        M_max: [usize; N_PARAM_LAYERS],
        ef_constr_search: [usize; N_PARAM_LAYERS],
        ef_constr_insert: [usize; N_PARAM_LAYERS],
        ef_search: [usize; N_PARAM_LAYERS],
        layer_probability: f64,
    ) -> Self {
        let params = HawkerParams {
            M,
            M_max,
            ef_constr_search,
            ef_constr_insert,
            ef_search,
            layer_probability,
        };
        Self(HawkSearcher { params })
    }

    pub fn insert(
        &self,
        iris: PyIrisCode,
        vector: &mut PyPlaintextStore,
        graph: &mut PyGraphStore,
    ) -> u32 {
        let id = py_bindings::hnsw::insert(iris.0, &self.0, &mut vector.0, &mut graph.0);
        id.0
    }

    pub fn insert_uniform_random(
        &self,
        vector: &mut PyPlaintextStore,
        graph: &mut PyGraphStore,
    ) -> u32 {
        let id = py_bindings::hnsw::insert_uniform_random(&self.0, &mut vector.0, &mut graph.0);
        id.0
    }

    pub fn fill_uniform_random(
        &self,
        num: usize,
        vector: &mut PyPlaintextStore,
        graph: &mut PyGraphStore,
    ) {
        py_bindings::hnsw::fill_uniform_random(num, &self.0, &mut vector.0, &mut graph.0);
    }

    #[pyo3(signature = (filename, vector, graph, limit=None))]
    pub fn fill_from_ndjson_file(
        &self,
        filename: String,
        vector: &mut PyPlaintextStore,
        graph: &mut PyGraphStore,
        limit: Option<usize>,
    ) {
        py_bindings::hnsw::fill_from_ndjson_file(
            &filename,
            limit,
            &self.0,
            &mut vector.0,
            &mut graph.0,
        );
    }

    /// Search HNSW index and return nearest ID and its distance from query
    pub fn search(
        &mut self,
        query: &PyIrisCode,
        vector: &mut PyPlaintextStore,
        graph: &mut PyGraphStore,
    ) -> (u32, f64) {
        let (id, dist) =
            py_bindings::hnsw::search(query.0.clone(), &self.0, &mut vector.0, &mut graph.0);
        (id.0, dist)
    }

    #[staticmethod]
    pub fn read_from_json(filename: String) -> PyResult<Self> {
        let result = py_bindings::io::read_json(&filename)
            .map_err(|_| PyIOError::new_err("Unable to read from file"))?;
        Ok(Self(result))
    }

    pub fn write_to_json(&self, filename: String) -> PyResult<()> {
        py_bindings::io::write_json(&self.0, &filename)
            .map_err(|_| PyIOError::new_err("Unable to write to file"))
    }
}
