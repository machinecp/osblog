// gpu.rs
// Graphics stuff
// Stephen Marz
// 12 May 2020

#![allow(dead_code)]
use crate::{page::{zalloc, PAGE_SIZE},
            virtio,
            virtio::{MmioOffsets, Queue, StatusField, VIRTIO_RING_SIZE}};
use core::{mem::size_of, ptr::null_mut};

pub const F_VIRGL: u32 = 0;
pub const F_EDID: u32 = 1;

pub const EVENT_DISPLAY: u32 = 1 << 0;
#[repr(C)]
pub struct Config {
	//events_read signals pending events to the driver. The driver MUST NOT write to this field.
	// events_clear clears pending events in the device. Writing a ’1’ into a bit will clear the corresponding bit in events_read mimicking write-to-clear behavior.
	//num_scanouts specifies the maximum number of scanouts supported by the device. Minimum value is 1, maximum value is 16.
	events_read: u32,
	events_clear: u32,
	num_scanouts: u32,
	reserved: u32,
}
#[repr(u32)]
pub enum CtrlType {
	/* 2d commands */
	CmdGetDisplayInfo = 0x0100,
	CmdResourceCreate2d,
	CmdResourceUref,
	CmdSetScanout,
	CmdResourceFlush,
	CmdTransferToHost2d,
	CmdResourceAttachBacking,
	CmdResourceDetachBacking,
	CmdGetCapsetInfo,
	CmdGetCapset,
	CmdGetEdid,
	/* cursor commands */
	CmdUpdateCursor = 0x0300,
	CmdMoveCursor,
	/* success responses */
	RespOkNoData = 0x1100,
	RespOkDisplayInfo,
	RespOkCapsetInfo,
	RespOkCapset,
	RespOkEdid,
	/* error responses */
	RespErrUnspec = 0x1200,
	RespErrOutOfMemory,
	RespErrInvalidScanoutId,
	RespErrInvalidResourceId,
	RespErrInvalidContextId,
	RespErrInvalidParameter,
}

pub const FLAG_FENCE: u32= 1 << 0;
#[repr(C)]
pub struct CtrlHeader {
	ctrl_type: CtrlType,
	flags: u32,
	fence_id: u64,
	ctx_id: u32,
	padding: u32
}

pub const MAX_SCANOUTS: usize = 16;
#[repr(C)]
pub struct Rect {
	x: u32,
	y: u32,
	width: u32,
	height: u32,
}
#[repr(C)]
pub struct DisplayOne {
	r: Rect,
	enabled: u32,
	flags: u32,
}

#[repr(C)]
pub struct RespDisplayInfo {
	hdr: CtrlHeader,
	pmodes: [DisplayOne; MAX_SCANOUTS],
}
#[repr(C)]
pub struct GetEdid {
	hdr: CtrlHeader,
	scanout: u32,
	padding: u32,
}
#[repr(C)]
pub struct RespEdid {
	hdr: CtrlHeader,
	size: u32,
	padding: u32,
	edid: [u8; 1024],
}
#[repr(u32)]
pub enum Formats {
	B8G8R8A8Unorm = 1,
	B8G8R8X8Unorm = 2,
	A8R8G8B8Unorm = 3,
	X8R8G8B8Unorm = 4,
	R8G8B8A8Unorm = 67,
	X8B8G8R8Unorm = 68,
	A8B8G8R8Unorm = 121,
	R8G8B8X8Unorm = 134,
}

#[repr(C)]
pub struct ResourceCreate2d {
	hdr: CtrlHeader,
	resource_id: u32,
	format: Formats,
	width: u32,
	height: u32,
}
#[repr(C)]
pub struct ResourceUnref {
	hdr: CtrlHeader,
	resource_id: u32,
	padding: u32,
}
#[repr(C)]
pub struct SetScanout {
	hdr: CtrlHeader,
	r: Rect,
	scanout_id: u32,
	resource_id: u32,
}
#[repr(C)]
pub struct ResourceFlush {
	hdr: CtrlHeader,
	r: Rect,
	resource_id: u32,
	padding: u32,
}

#[repr(C)]
pub struct TransferToHost2d {
	hdr: CtrlHeader,
	r: Rect,
	offset: u64,
	resource_id: u32,
	padding: u32,
}
#[repr(C)]
pub struct AttachBacking {
	hdr: CtrlHeader,
	resource_id: u32,
	nr_entries: u32,
}

#[repr(C)]
pub struct MemEntry {
	addr: u64,
	length: u32,
	padding: u32,
}

#[repr(C)]
pub struct DetachBacking {
	hdr: CtrlHeader,
	resource_id: u32,
	padding: u32,
}
#[repr(C)]
pub struct CursorPos {
	scanout_id: u32,
	x: u32,
	y: u32,
	padding: u32,
}

#[repr(C)]
pub struct UpdateCursor {
	hdr: CtrlHeader,
	pos: CursorPos,
	resource_id: u32,
	hot_x: u32,
	hot_y: u32,
	padding: u32,
}



pub struct Device {
	queue:        *mut Queue,
	dev:          *mut u32,
	idx:          u16,
	ack_used_idx: u16,
}
impl Device {
	pub const fn new() -> Self {
		Self { queue:        null_mut(),
		       dev:          null_mut(),
		       idx:          0,
		       ack_used_idx: 0, }
	}
}

static mut GPU_DEVICES: [Option<Device>; 8] = [
	None,
	None,
	None,
	None,
	None,
	None,
	None,
	None,
];

pub fn setup_gpu_device(ptr: *mut u32) -> bool {
	unsafe {
		// We can get the index of the device based on its address.
		// 0x1000_1000 is index 0
		// 0x1000_2000 is index 1
		// ...
		// 0x1000_8000 is index 7
		// To get the number that changes over, we shift right 12 places (3 hex digits)
		let idx = (ptr as usize - virtio::MMIO_VIRTIO_START) >> 12;
		// [Driver] Device Initialization
		// 1. Reset the device (write 0 into status)
		ptr.add(MmioOffsets::Status.scale32()).write_volatile(0);
		let mut status_bits = StatusField::Acknowledge.val32();
		// 2. Set ACKNOWLEDGE status bit
		ptr.add(MmioOffsets::Status.scale32()).write_volatile(status_bits);
		// 3. Set the DRIVER status bit
		status_bits |= StatusField::DriverOk.val32();
		ptr.add(MmioOffsets::Status.scale32()).write_volatile(status_bits);
		// 4. Read device feature bits, write subset of feature
		// bits understood by OS and driver    to the device.
		let host_features = ptr.add(MmioOffsets::HostFeatures.scale32()).read_volatile();
		ptr.add(MmioOffsets::GuestFeatures.scale32()).write_volatile(host_features);
		// 5. Set the FEATURES_OK status bit
		status_bits |= StatusField::FeaturesOk.val32();
		ptr.add(MmioOffsets::Status.scale32()).write_volatile(status_bits);
		// 6. Re-read status to ensure FEATURES_OK is still set.
		// Otherwise, it doesn't support our features.
		let status_ok = ptr.add(MmioOffsets::Status.scale32()).read_volatile();
		// If the status field no longer has features_ok set,
		// that means that the device couldn't accept
		// the features that we request. Therefore, this is
		// considered a "failed" state.
		if false == StatusField::features_ok(status_ok) {
			print!("features fail...");
			ptr.add(MmioOffsets::Status.scale32()).write_volatile(StatusField::Failed.val32());
			return false;
		}
		// 7. Perform device-specific setup.
		// Set the queue num. We have to make sure that the
		// queue size is valid because the device can only take
		// a certain size.
		let qnmax = ptr.add(MmioOffsets::QueueNumMax.scale32()).read_volatile();
		ptr.add(MmioOffsets::QueueNum.scale32()).write_volatile(VIRTIO_RING_SIZE as u32);
		if VIRTIO_RING_SIZE as u32 > qnmax {
			print!("queue size fail...");
			return false;
		}
		// First, if the block device array is empty, create it!
		// We add 4095 to round this up and then do an integer
		// divide to truncate the decimal. We don't add 4096,
		// because if it is exactly 4096 bytes, we would get two
		// pages, not one.
		let num_pages = (size_of::<Queue>() + PAGE_SIZE - 1) / PAGE_SIZE;
		// println!("np = {}", num_pages);
		// We allocate a page for each device. This will the the
		// descriptor where we can communicate with the block
		// device. We will still use an MMIO register (in
		// particular, QueueNotify) to actually tell the device
		// we put something in memory. We also have to be
		// careful with memory ordering. We don't want to
		// issue a notify before all memory writes have
		// finished. We will look at that later, but we need
		// what is called a memory "fence" or barrier.
		ptr.add(MmioOffsets::QueueSel.scale32()).write_volatile(0);
		// Alignment is very important here. This is the memory address
		// alignment between the available and used rings. If this is wrong,
		// then we and the device will refer to different memory addresses
		// and hence get the wrong data in the used ring.
		// ptr.add(MmioOffsets::QueueAlign.scale32()).write_volatile(2);
		let queue_ptr = zalloc(num_pages) as *mut Queue;
		let queue_pfn = queue_ptr as u32;
		ptr.add(MmioOffsets::GuestPageSize.scale32()).write_volatile(PAGE_SIZE as u32);
		// QueuePFN is a physical page number, however it
		// appears for QEMU we have to write the entire memory
		// address. This is a physical memory address where we
		// (the OS) and the block device have in common for
		// making and receiving requests.
		ptr.add(MmioOffsets::QueuePfn.scale32()).write_volatile(queue_pfn / PAGE_SIZE as u32);
		// 8. Set the DRIVER_OK status bit. Device is now "live"
		status_bits |= StatusField::DriverOk.val32();
		ptr.add(MmioOffsets::Status.scale32()).write_volatile(status_bits);

		let dev = Device {
			queue: queue_ptr,
			dev: ptr,
			idx: 0,
			ack_used_idx: 0,
		};

		GPU_DEVICES[idx] = Some(dev);

		true
	}
}

