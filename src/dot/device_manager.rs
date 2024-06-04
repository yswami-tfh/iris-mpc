use std::sync::Arc;

use cudarc::{
    cublas::CudaBlas,
    driver::{
        result::{
            self, event, malloc_async, memcpy_htod_async, stream::{synchronize, wait_event}
        }, sys::{CUevent, CUevent_flags}, CudaDevice, CudaSlice, CudaStream, DevicePtr, DeviceRepr
    },
};

#[derive(Clone)]
pub struct DeviceManager {
    devices: Vec<Arc<CudaDevice>>,
}

impl DeviceManager {
    pub fn init() -> Self {
        let mut devices = vec![];
        for i in 0..CudaDevice::count().unwrap() {
            devices.push(CudaDevice::new(i as usize).unwrap());
        }
        Self { devices }
    }

    pub fn fork_streams(&self) -> Vec<CudaStream> {
        self.devices
            .iter()
            .map(|dev| dev.fork_default_stream().unwrap())
            .collect::<Vec<_>>()
    }

    pub fn create_cublas(&self, streams: &Vec<CudaStream>) -> Vec<CudaBlas> {
        self.devices
            .iter()
            .zip(streams)
            .map(|(dev, stream)| {
                let blas = CudaBlas::new(dev.clone()).unwrap();
                unsafe {
                    blas.set_stream(Some(stream)).unwrap();
                }
                blas
            })
            .collect::<Vec<_>>()
    }

    pub fn await_streams(&self, streams: &Vec<CudaStream>) {
        for i in 0..self.devices.len() {
            self.devices[i].bind_to_thread().unwrap();
            unsafe { synchronize(streams[i].stream).unwrap() }
        }
    }

    pub fn create_events(&self) -> Vec<CUevent> {
        let mut events = vec![];
        for idx in 0..self.devices.len() {
            self.devices[idx].bind_to_thread().unwrap();
            events.push(event::create(CUevent_flags::CU_EVENT_DEFAULT).unwrap());
        }
        events
    }

    pub fn record_event(&self, streams: &Vec<CudaStream>, events: &Vec<CUevent>) {
        for idx in 0..self.devices.len() {
            unsafe {
                self.devices[idx].bind_to_thread().unwrap();
                event::record(events[idx], streams[idx].stream).unwrap();
            };
        }
    }

    pub fn await_event(&self, streams: &Vec<CudaStream>, events: &Vec<CUevent>) {
        for idx in 0..self.devices.len() {
            unsafe {
                self.devices[idx].bind_to_thread().unwrap();
                wait_event(
                    streams[idx].stream,
                    events[idx],
                    cudarc::driver::sys::CUevent_wait_flags::CU_EVENT_WAIT_DEFAULT,
                )
                .unwrap();
            };
        }
    }

    pub fn htod_transfer_query(&self, preprocessed_query: &Vec<Vec<u8>>, streams: &Vec<CudaStream>) -> (Vec<u64>, Vec<u64>) {
        let mut query0_ptrs = vec![];
        let mut query1_ptrs = vec![];
        for idx in 0..self.device_count() {
            self.device(idx).bind_to_thread().unwrap();
            let query0 =
                unsafe { malloc_async(streams[idx].stream, preprocessed_query[0].len()).unwrap() };
            unsafe {
                memcpy_htod_async(query0, &preprocessed_query[0], streams[idx].stream).unwrap();
            }

            let query1 =
                unsafe { malloc_async(streams[idx].stream, preprocessed_query[1].len()).unwrap() };
            unsafe {
                memcpy_htod_async(query1, &preprocessed_query[1], streams[idx].stream).unwrap();
            }

            query0_ptrs.push(query0);
            query1_ptrs.push(query1);
        }
        (query0_ptrs, query1_ptrs)
    }

    pub fn device(&self, index: usize) -> Arc<CudaDevice> {
        self.devices[index].clone()
    }

    pub fn devices(&self) -> &Vec<Arc<CudaDevice>> {
        &self.devices
    }

    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    pub fn htod_copy_into<T: DeviceRepr + Unpin>(
        &self,
        src: Vec<T>,
        dst: &mut CudaSlice<T>,
        index: usize,
    ) -> Result<(), result::DriverError> {
        self.device(index).bind_to_thread()?;
        unsafe { result::memcpy_htod_sync(*dst.device_ptr(), src.as_ref())? };
        Ok(())
    }
}
