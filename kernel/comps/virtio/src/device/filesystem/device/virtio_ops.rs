// SPDX-License-Identifier: MPL-2.0

use super::*;

impl FileSystemDevice {
    pub fn negotiate_features(features: u64) -> u64 {
        let device_features = FileSystemFeatures::from_bits_truncate(features);
        let supported_features = FileSystemFeatures::supported_features();
        let fs_features = device_features & supported_features;
        debug!("features negotiated: {:?}", fs_features);
        fs_features.bits()
    }

    pub(crate) fn init(mut transport: Box<dyn VirtioTransport>) -> Result<(), VirtioDeviceError> {
        let config_manager = VirtioFsConfig::new_manager(transport.as_ref());
        let config = config_manager.read_config();
        let parsed_tag = Self::parse_tag(&config.tag);

        let notify_supported =
            transport.read_device_features() & FileSystemFeatures::NOTIFICATION.bits() != 0;
        let special_queues_count = if notify_supported { 2 } else { 1 };

        let total_queues = transport.num_queues();
        let max_request_queues_from_transport =
            total_queues.saturating_sub(special_queues_count) as usize;
        let request_queue_count = cmp::min(
            config.num_request_queues as usize,
            max_request_queues_from_transport,
        );

        warn!(
            "[virtiofs-debug] init start parsed_tag='{}' raw_tag={:?} total_queues={} num_request_queues={} special_queues_count={} notify_supported={}",
            parsed_tag,
            &config.tag,
            total_queues,
            config.num_request_queues,
            special_queues_count,
            notify_supported
        );

        if request_queue_count == 0 {
            warn!(
                "[virtiofs-debug] request queue count is zero: total_queues={} special_queues_count={} config.num_request_queues={}",
                total_queues, special_queues_count, config.num_request_queues
            );
            return Err(VirtioDeviceError::QueuesAmountDoNotMatch(
                total_queues,
                special_queues_count + config.num_request_queues as u16,
            ));
        }

        let hiprio_queue =
            FsRequestQueue::new(Self::new_queue(HIPRIO_QUEUE_INDEX, transport.as_mut())?);

        let dma_pools = FsDmaPools::new();

        let mut request_queues = Vec::with_capacity(request_queue_count);
        for idx in 0..request_queue_count {
            let queue_index = special_queues_count + idx as u16;
            request_queues.push(FsRequestQueue::new(Self::new_queue(
                queue_index,
                transport.as_mut(),
            )?));
        }

        let device = Arc::new(Self {
            transport: SpinLock::new(transport),
            hiprio_queue,
            request_queues,
            dma_pools,
            unique_id_alloc: SyncIdAlloc::with_capacity(UNIQUE_ID_ALLOC_CAPACITY),
            tag: parsed_tag,
            notify_supported,
        });

        let mut transport = device.transport.lock();
        transport
            .register_cfg_callback(Box::new(config_space_change))
            .unwrap();
        let device_for_hiprio_callback = device.clone();
        let hiprio_wakeup_callback = move |_: &TrapFrame| {
            device_for_hiprio_callback.handle_queue_irq(QueueSelector::Hiprio);
        };
        transport
            .register_queue_callback(HIPRIO_QUEUE_INDEX, Box::new(hiprio_wakeup_callback), false)
            .unwrap();
        for idx in 0..request_queue_count {
            let queue_idx = special_queues_count + idx as u16;
            let device_for_callback = device.clone();
            let wakeup_callback = move |_: &TrapFrame| {
                device_for_callback.handle_queue_irq(QueueSelector::Request(
                    (queue_idx - special_queues_count) as usize,
                ));
            };
            transport
                .register_queue_callback(queue_idx, Box::new(wakeup_callback), false)
                .unwrap();
        }
        transport.finish_init();
        drop(transport);

        device.fuse_init()?;
        warn!("[virtiofs-debug] fuse init ok for tag='{}'", device.tag);

        let devices = FILESYSTEM_DEVICES.call_once(|| SpinLock::new(Vec::new()));
        let mut devices = devices.disable_irq().lock();
        devices.push(device.clone());
        let registered_tags: Vec<&str> = devices.iter().map(|device| device.tag.as_str()).collect();
        warn!(
            "[virtiofs-debug] registered tags after init: {:?}",
            registered_tags
        );
        drop(devices);

        info!(
            "{} initialized, tag = {}, request_queues = {}, notify = {}",
            DEVICE_NAME,
            device.tag,
            device.request_queues.len(),
            device.notify_supported
        );
        info!(
            "{} test file read is deferred; call debug_read_test_file_for_all_devices() later",
            DEVICE_NAME
        );

        let _ = &device.hiprio_queue;
        let _ = &device.request_queues;

        Ok(())
    }

    fn new_queue(
        index: u16,
        transport: &mut dyn VirtioTransport,
    ) -> Result<VirtQueue, VirtioDeviceError> {
        let max_queue_size = transport.max_queue_size(index).map_err(map_transport_err)?;
        let queue_size = cmp::min(DEFAULT_QUEUE_SIZE, max_queue_size);
        if queue_size == 0 {
            return Err(VirtioDeviceError::QueueUnknownError);
        }
        VirtQueue::new(index, queue_size, transport).map_err(Into::into)
    }

    pub(super) fn queue(&self, selector: QueueSelector) -> &FsRequestQueue {
        match selector {
            QueueSelector::Hiprio => &self.hiprio_queue,
            QueueSelector::Request(index) => &self.request_queues[index],
        }
    }

    pub(super) fn select_request_queue(&self, nodeid: u64) -> QueueSelector {
        let request_queue_count = self.request_queues.len();
        if request_queue_count <= 1 {
            return QueueSelector::Request(0);
        }

        QueueSelector::Request((nodeid as usize) % request_queue_count)
    }

    pub(super) fn submit_request(
        &self,
        selector: QueueSelector,
        unique: u64,
        in_slices: &[&Slice<FsDmaBuf>],
        out_slices: &[&Slice<FsDmaBuf>],
    ) -> Result<(), VirtioDeviceError> {
        let queue = self.queue(selector);

        {
            let mut virt_queue = queue.queue.lock();

            let token = virt_queue.add_dma_bufs(in_slices, out_slices)?;

            queue
                .pending_requests
                .disable_irq()
                .lock()
                .insert(token, unique as usize);

            if virt_queue.should_notify() {
                virt_queue.notify();
            }
        }
        Ok(())
    }

    pub(super) fn submit_request_and_wait(
        &self,
        selector: QueueSelector,
        unique: u64,
        in_slices: &[&Slice<FsDmaBuf>],
        out_slices: &[&Slice<FsDmaBuf>],
    ) -> Result<(), VirtioDeviceError> {
        self.submit_request(selector, unique, in_slices, out_slices)?;
        self.wait_for_unique(selector, unique as usize)
    }

    pub(super) fn check_reply(
        &self,
        out_header_slice: &Slice<FsDmaBuf>,
        unique: u64,
        map_fs_error: bool,
    ) -> Result<OutHeader, VirtioDeviceError> {
        out_header_slice
            .mem_obj()
            .sync_from_device(out_header_slice.offset().clone())
            .unwrap();
        let out_header: OutHeader = out_header_slice.read_val(0).unwrap();
        if out_header.unique != unique || out_header.error != 0 {
            warn!(
                "{} failed: unique={}, error={}, out_len={}",
                DEVICE_NAME, out_header.unique, out_header.error, out_header.len
            );
            if map_fs_error && out_header.unique == unique && out_header.error != 0 {
                return Err(VirtioDeviceError::FileSystemError(out_header.error));
            }
            return Err(VirtioDeviceError::QueueUnknownError);
        }

        Ok(out_header)
    }

    pub(super) fn handle_queue_irq(&self, selector: QueueSelector) {
        let queue_state = self.queue(selector);
        loop {
            let pop_result = {
                let mut queue = queue_state.queue.lock();
                queue.pop_used()
            };

            let (token, _) = match pop_result {
                Ok(completed) => completed,
                Err(PopUsedError::NotReady) => break,
            };

            let pending = queue_state
                .pending_requests
                .disable_irq()
                .lock()
                .remove(&token);

            let Some(pending) = pending else {
                continue;
            };

            let mut request_states = queue_state.request_states.disable_irq().lock();
            let request_state = request_states.entry(pending).or_insert(RequestWaitState {
                completed: false,
                waker: None,
            });
            request_state.completed = true;

            if let Some(waker) = request_state.waker.take() {
                let _ = waker.wake_up();
            }
        }
    }

    pub(super) fn wait_for_unique(
        &self,
        selector: QueueSelector,
        unique: usize,
    ) -> Result<(), VirtioDeviceError> {
        let queue_state = self.queue(selector);

        {
            let mut request_states = queue_state.request_states.disable_irq().lock();
            if let Some(state) = request_states.get(&unique)
                && state.completed
            {
                request_states.remove(&unique);
                self.unique_id_alloc.dealloc(unique);
                return Ok(());
            }
        }

        let (waiter, waker) = Waiter::new_pair();
        {
            let mut request_states = queue_state.request_states.disable_irq().lock();
            let state = request_states.entry(unique).or_insert(RequestWaitState {
                completed: false,
                waker: None,
            });
            if state.completed {
                request_states.remove(&unique);
                self.unique_id_alloc.dealloc(unique);
                return Ok(());
            }
            state.waker = Some(waker);
        }

        let timeout_deadline = Jiffies::elapsed()
            .as_u64()
            .saturating_add(REQUEST_WAIT_TIMEOUT_JIFFIES);

        let wait_res = waiter.wait_until_or_cancelled(
            || {
                let mut request_states = queue_state.request_states.disable_irq().lock();
                if let Some(state) = request_states.get(&unique)
                    && state.completed
                {
                    request_states.remove(&unique);
                    return Some(());
                }
                None
            },
            || {
                if Jiffies::elapsed().as_u64() >= timeout_deadline {
                    Err(())
                } else {
                    Ok(())
                }
            },
        );

        if wait_res.is_ok() {
            self.unique_id_alloc.dealloc(unique);
            return Ok(());
        }

        let mut request_states = queue_state.request_states.disable_irq().lock();
        if let Some(state) = request_states.get_mut(&unique) {
            state.waker = None;
        }
        request_states.remove(&unique);
        self.unique_id_alloc.dealloc(unique);

        Err(VirtioDeviceError::QueueUnknownError)
    }

    /// Wait for a reply from the device by spinning;
    /// intended for early boot or non-task contexts.
    pub(super) fn wait_for_unique_early(
        &self,
        selector: QueueSelector,
        unique: usize,
    ) -> Result<(), VirtioDeviceError> {
        let queue_state = self.queue(selector);

        loop {
            self.handle_queue_irq(selector);

            let mut request_states = queue_state.request_states.disable_irq().lock();
            if let Some(state) = request_states.get(&unique)
                && state.completed
            {
                request_states.remove(&unique);
                self.unique_id_alloc.dealloc(unique);
                return Ok(());
            }

            spin_loop();
        }
    }
}
