//! The Series barrier groups have the desktop layout with guest COM resource pointers.
use super::*;

fn guest_pointer(
    field: &mem::ManuallyDrop<Option<ID3D12Resource>>,
) -> Result<*mut c_void, HRESULT> {
    field.as_ref().map(Interface::as_raw).ok_or(E_INVALIDARG)
}

fn borrow_resource(field: &mut mem::ManuallyDrop<Option<ID3D12Resource>>, native: &ID3D12Resource) {
    // Desktop D3D12 borrows the COM pointer for the duration of the call.
    unsafe {
        ptr::from_mut(field)
            .cast::<*mut c_void>()
            .write(native.as_raw())
    };
}

fn texture_group(api: &ID3D12GraphicsCommandList7, items: &[D3D12_TEXTURE_BARRIER]) {
    if !items.is_empty() {
        unsafe {
            api.Barrier(&[D3D12_BARRIER_GROUP {
                Type: D3D12_BARRIER_TYPE_TEXTURE,
                NumBarriers: items.len() as u32,
                Anonymous: D3D12_BARRIER_GROUP_0 {
                    pTextureBarriers: items.as_ptr(),
                },
            }]);
        }
    }
}

unsafe fn record(
    owner: *mut c_void,
    native: &ID3D12GraphicsCommandList7,
    groups: &[D3D12_BARRIER_GROUP],
) -> Result<(), HRESULT> {
    for group in groups {
        let count = group.NumBarriers as usize;
        if count == 0 || unsafe { group.Anonymous.pGlobalBarriers.is_null() } {
            return Err(E_INVALIDARG);
        }
        match group.Type {
            D3D12_BARRIER_TYPE_GLOBAL => unsafe { native.Barrier(slice::from_ref(group)) },
            D3D12_BARRIER_TYPE_BUFFER => {
                let mut barriers = Vec::new();
                barriers
                    .try_reserve_exact(count)
                    .map_err(|_| E_OUTOFMEMORY)?;
                for item in unsafe { slice::from_raw_parts(group.Anonymous.pBufferBarriers, count) }
                {
                    let resource =
                        unsafe { device::resource_proxy(guest_pointer(&item.pResource)?, owner) }
                            .ok_or(E_INVALIDARG)?;
                    let mut item = unsafe { ptr::read(item) };
                    if let Some((delta, len)) = resource.state.buffer_window() {
                        let size = if item.Size == u64::MAX && item.Offset == 0 {
                            len
                        } else {
                            item.Size
                        };
                        if size == 0 || item.Offset.checked_add(size).is_none_or(|end| end > len) {
                            return Err(E_INVALIDARG);
                        }
                        let host_end = delta
                            .checked_add(item.Offset)
                            .and_then(|offset| offset.checked_add(size))
                            .ok_or(E_INVALIDARG)?;
                        if host_end > unsafe { resource.native.GetDesc() }.Width {
                            return Err(E_INVALIDARG);
                        }
                        if item.SyncBefore == D3D12_BARRIER_SYNC_SPLIT
                            || item.SyncAfter == D3D12_BARRIER_SYNC_SPLIT
                            || item.AccessBefore == D3D12_BARRIER_ACCESS_NO_ACCESS
                            || item.AccessAfter == D3D12_BARRIER_ACCESS_NO_ACCESS
                        {
                            return Err(E_NOTIMPL);
                        }
                        // Desktop buffer barriers require the whole resource. A
                        // wider memory dependency also orders the guest window.
                        // Splitting or deactivating it could affect a neighbour.
                        item.Offset = 0;
                        item.Size = u64::MAX;
                    }
                    borrow_resource(&mut item.pResource, &resource.native);
                    barriers.push(item);
                }
                unsafe {
                    native.Barrier(&[D3D12_BARRIER_GROUP {
                        Anonymous: D3D12_BARRIER_GROUP_0 {
                            pBufferBarriers: barriers.as_ptr(),
                        },
                        ..*group
                    }]);
                }
            }
            D3D12_BARRIER_TYPE_TEXTURE => {
                let mut barriers = Vec::new();
                barriers
                    .try_reserve_exact(count)
                    .map_err(|_| E_OUTOFMEMORY)?;
                for item in
                    unsafe { slice::from_raw_parts(group.Anonymous.pTextureBarriers, count) }
                {
                    let resource =
                        unsafe { device::resource_proxy(guest_pointer(&item.pResource)?, owner) }
                            .ok_or(E_INVALIDARG)?;
                    if let Some(pair) = resource.state.depth() {
                        // Copies belong between the preceding and following barriers.
                        texture_group(native, &barriers);
                        barriers.clear();
                        if depth::enhanced_handoff(native, &resource.native, pair, item)
                            .map_err(|error| error.code())?
                        {
                            continue;
                        }
                    }
                    let mut item = unsafe { ptr::read(item) };
                    borrow_resource(&mut item.pResource, &resource.native);
                    if resource.state.unbacked_uav() {
                        // The host depth image has no unordered access; see unbacked_uav.
                        item.LayoutBefore = device::format::host_layout(item.LayoutBefore);
                        item.LayoutAfter = device::format::host_layout(item.LayoutAfter);
                        item.AccessBefore &= !D3D12_BARRIER_ACCESS_UNORDERED_ACCESS;
                        item.AccessAfter &= !D3D12_BARRIER_ACCESS_UNORDERED_ACCESS;
                    }
                    if resource.state.placed().is_some() {
                        // Simultaneous-access textures share COMMON across all uses.
                        if item.LayoutBefore != D3D12_BARRIER_LAYOUT_UNDEFINED {
                            item.LayoutBefore = D3D12_BARRIER_LAYOUT_COMMON;
                        }
                        if item.LayoutAfter != D3D12_BARRIER_LAYOUT_UNDEFINED {
                            item.LayoutAfter = D3D12_BARRIER_LAYOUT_COMMON;
                        }
                    }
                    barriers.push(item);
                }
                texture_group(native, &barriers);
            }
            _ => return Err(E_NOTIMPL),
        }
    }
    Ok(())
}

enum Group {
    Global(Vec<D3D12_GLOBAL_BARRIER>),
    Buffer(Vec<D3D12_BUFFER_BARRIER>),
    Texture(Vec<D3D12_TEXTURE_BARRIER>),
}

pub(super) struct Batch {
    owner: IUnknown,
    groups: Vec<Group>,
    // The D3D12 structs hold borrowed guest pointers in ManuallyDrop fields.
    resources: Vec<IUnknown>,
}

impl Batch {
    // Split barriers and depth handoff copies stay on the original list.
    unsafe fn capture(
        list: &List,
        groups: &[D3D12_BARRIER_GROUP],
    ) -> windows::core::Result<Option<Self>> {
        let mut batch = Self {
            owner: list.parent.clone().ok_or(E_INVALIDARG)?,
            groups: Vec::new(),
            resources: Vec::new(),
        };
        batch.groups.try_reserve_exact(groups.len()).map_err(|_| E_OUTOFMEMORY)?;
        for group in groups {
            let count = group.NumBarriers as usize;
            if count == 0 || unsafe { group.Anonymous.pGlobalBarriers.is_null() } {
                return Err(E_INVALIDARG.into());
            }
            let captured = match group.Type {
                D3D12_BARRIER_TYPE_GLOBAL => {
                    let items = unsafe { slice::from_raw_parts(group.Anonymous.pGlobalBarriers, count) };
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(count).map_err(|_| E_OUTOFMEMORY)?;
                    copy.extend_from_slice(items);
                    Group::Global(copy)
                }
                D3D12_BARRIER_TYPE_BUFFER => {
                    let items = unsafe { slice::from_raw_parts(group.Anonymous.pBufferBarriers, count) };
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(count).map_err(|_| E_OUTOFMEMORY)?;
                    for item in items {
                        if item.SyncBefore == D3D12_BARRIER_SYNC_SPLIT || item.SyncAfter == D3D12_BARRIER_SYNC_SPLIT {
                            return Ok(None);
                        }
                        batch.retain(&item.pResource)?;
                        copy.push(unsafe { ptr::read(item) });
                    }
                    Group::Buffer(copy)
                }
                D3D12_BARRIER_TYPE_TEXTURE => {
                    let items = unsafe { slice::from_raw_parts(group.Anonymous.pTextureBarriers, count) };
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(count).map_err(|_| E_OUTOFMEMORY)?;
                    for item in items {
                        if item.SyncBefore == D3D12_BARRIER_SYNC_SPLIT || item.SyncAfter == D3D12_BARRIER_SYNC_SPLIT {
                            return Ok(None);
                        }
                        let resource = batch.retain(&item.pResource)?;
                        if resource.state.depth().is_some() {
                            return Ok(None);
                        }
                        copy.push(unsafe { ptr::read(item) });
                    }
                    Group::Texture(copy)
                }
                _ => return Err(E_NOTIMPL.into()),
            };
            batch.groups.push(captured);
        }
        Ok(Some(batch))
    }

    fn retain(
        &mut self,
        field: &mem::ManuallyDrop<Option<ID3D12Resource>>,
    ) -> windows::core::Result<&device::Resource> {
        let guest = guest_pointer(field)?;
        // SAFETY: capture runs while the caller holds the guest resource.
        let resource = unsafe { device::resource_proxy(guest, self.owner.as_raw()) }.ok_or(E_INVALIDARG)?;
        self.resources.try_reserve(1).map_err(|_| E_OUTOFMEMORY)?;
        let borrowed = unsafe { IUnknown::from_raw_borrowed(&guest) }.ok_or(E_INVALIDARG)?;
        self.resources.push(borrowed.clone());
        Ok(resource)
    }

    pub(super) fn emit(&self, list: &ID3D12GraphicsCommandList) -> windows::core::Result<()> {
        let native = list.cast::<ID3D12GraphicsCommandList7>()?;
        for group in &self.groups {
            let raw = match group {
                Group::Global(items) => D3D12_BARRIER_GROUP {
                    Type: D3D12_BARRIER_TYPE_GLOBAL,
                    NumBarriers: items.len() as u32,
                    Anonymous: D3D12_BARRIER_GROUP_0 { pGlobalBarriers: items.as_ptr() },
                },
                Group::Buffer(items) => D3D12_BARRIER_GROUP {
                    Type: D3D12_BARRIER_TYPE_BUFFER,
                    NumBarriers: items.len() as u32,
                    Anonymous: D3D12_BARRIER_GROUP_0 { pBufferBarriers: items.as_ptr() },
                },
                Group::Texture(items) => D3D12_BARRIER_GROUP {
                    Type: D3D12_BARRIER_TYPE_TEXTURE,
                    NumBarriers: items.len() as u32,
                    Anonymous: D3D12_BARRIER_GROUP_0 { pTextureBarriers: items.as_ptr() },
                },
            };
            unsafe { record(self.owner.as_raw(), &native, slice::from_ref(&raw))? };
        }
        Ok(())
    }
}

pub(super) unsafe extern "system" fn barrier(
    list: &List,
    count: u32,
    groups: *const D3D12_BARRIER_GROUP,
) {
    let mut state = recording(list);
    if count == 0 || !state.ensure_open(&list.native) {
        return;
    }
    if groups.is_null() {
        state.error = E_INVALIDARG;
        return;
    }
    let groups = unsafe { slice::from_raw_parts(groups, count as usize) };
    if state.prefix.accepts_packets() {
        match unsafe { Batch::capture(list, groups) } {
            Ok(Some(batch)) => {
                if let Err(error) = state.prefix.push(&list.native, memory_wait::Packet::Barriers(batch)) {
                    state.error = error.code();
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                state.error = error.code();
                return;
            }
        }
    }
    if !state.can_record(&list.native) {
        return;
    }
    let Some(native) = list
        .state
        .enhanced_barriers
        .get_or_init(|| enhanced_barriers(&list.native))
    else {
        state.error = E_NOTIMPL;
        return;
    };
    if let Err(error) = unsafe { record(parent(&list.parent), native, groups) } {
        crate::diagnostics::recording_error(format_args!(
            "enhanced resource barrier group ({error:?})"
        ));
        state.error = error;
    }
}
