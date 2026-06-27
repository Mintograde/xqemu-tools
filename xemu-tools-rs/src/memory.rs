use crate::process::process_error_context;
use crate::qmp::QmpClient;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};

#[derive(Debug, Clone)]
struct CacheSegment {
    guest_start: u64,
    guest_end: u64,
    host_start: u64,
    data: Vec<u8>,
}

pub struct MemoryReader {
    pub pid: u32,
    handle: HANDLE,
    qmp: QmpClient,
    known_addresses: HashMap<u64, u64>,
    cache: Vec<CacheSegment>,
    ram_base_host: Option<u64>,
    pub read_counter: u64,
}

impl Drop for MemoryReader {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                CloseHandle(self.handle);
            }
        }
    }
}

impl MemoryReader {
    pub fn new(pid: u32, qmp: QmpClient) -> Result<Self> {
        let access = PROCESS_QUERY_LIMITED_INFORMATION
            | PROCESS_VM_READ
            | PROCESS_VM_WRITE
            | PROCESS_VM_OPERATION;
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err(process_error_context(pid));
        }
        Ok(Self {
            pid,
            handle,
            qmp,
            known_addresses: HashMap::new(),
            cache: Vec::new(),
            ram_base_host: None,
            read_counter: 0,
        })
    }

    pub fn reconnect_qmp(&mut self) -> Result<()> {
        self.qmp.reconnect()
    }

    pub fn clear_translations(&mut self) {
        self.known_addresses.clear();
        self.ram_base_host = None;
        self.invalidate_cache();
    }

    pub fn invalidate_cache(&mut self) {
        self.cache.clear();
    }

    pub fn populate_cache(&mut self) -> Result<()> {
        let game_state_base_address = self.read_u32(0x2E2D14)?;
        let game_state_size = self.read_u32(0x32E4A)?;
        self.add_to_cache(game_state_base_address as u64, game_state_size as usize)?;

        let global_scenario_address = self.read_u32(0x39BE5C)? as u64;
        let first_spawn_address = self.read_i32(global_scenario_address + 856)?;
        if first_spawn_address != 0 {
            let spawn_count = self.read_i32(global_scenario_address + 852)?;
            if spawn_count > 0 {
                self.add_to_cache(first_spawn_address as u64, 52 * spawn_count as usize)?;
            }
        }

        self.add_to_cache(0x271550, 688 * 4)?;
        self.add_to_cache(0x1FC0D0, (0x1FCBA4 - 0x1FC0D0) * 2)?;
        Ok(())
    }

    pub fn add_to_cache(&mut self, guest_start: u64, size: usize) -> Result<()> {
        if guest_start == 0 || size == 0 {
            return Ok(());
        }
        let host_start = self.get_host_address(guest_start)?;
        let data = self.read_host_bytes(host_start, size)?;
        self.cache.push(CacheSegment {
            guest_start,
            guest_end: guest_start + size as u64,
            host_start,
            data,
        });
        Ok(())
    }

    pub fn get_host_address(&mut self, guest: u64) -> Result<u64> {
        if let Some(host) = self.known_addresses.get(&guest) {
            return Ok(*host);
        }
        if let Some(host) = self.host_address_from_cache(guest) {
            self.known_addresses.insert(guest, host);
            return Ok(host);
        }
        let host = self.qmp.gva2hva(guest)?;
        self.known_addresses.insert(guest, host);
        Ok(host)
    }

    fn host_address_from_cache(&self, guest: u64) -> Option<u64> {
        self.cache.iter().find_map(|segment| {
            if segment.guest_start <= guest && guest < segment.guest_end {
                Some(segment.host_start + (guest - segment.guest_start))
            } else {
                None
            }
        })
    }

    fn cached_bytes(&mut self, guest: u64, len: usize) -> Option<(u64, Vec<u8>)> {
        for segment in &self.cache {
            if segment.guest_start <= guest && guest + len as u64 <= segment.guest_end {
                let offset = (guest - segment.guest_start) as usize;
                let host = segment.host_start + offset as u64;
                return Some((host, segment.data[offset..offset + len].to_vec()));
            }
        }
        None
    }

    fn read_guest_bytes(&mut self, guest: u64, len: usize) -> Result<Vec<u8>> {
        if let Some((host, data)) = self.cached_bytes(guest, len) {
            self.known_addresses.insert(guest, host);
            return Ok(data);
        }

        let host = if let Some(host) = self.known_addresses.get(&guest) {
            *host
        } else if guest > 0x8000_0000 {
            let base = match self.ram_base_host {
                Some(base) => base,
                None => {
                    let base = self.get_host_address(0x8000_0000)?;
                    self.ram_base_host = Some(base);
                    base
                }
            };
            let host = base + (guest - 0x8000_0000);
            self.known_addresses.insert(guest, host);
            host
        } else {
            self.get_host_address(guest)?
        };
        self.read_host_bytes(host, len)
    }

    pub fn read_host_bytes(&mut self, host: u64, len: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; len];
        let mut bytes_read = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                host as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                len,
                &mut bytes_read,
            )
        };
        if ok == 0 || bytes_read != len {
            let error = unsafe { GetLastError() };
            bail!(
                "ReadProcessMemory failed at host {host:#x} for {len} bytes (read {bytes_read}, error {error})"
            );
        }
        self.read_counter += 1;
        Ok(buffer)
    }

    #[allow(dead_code)]
    pub fn write_bytes(&mut self, guest: u64, bytes: &[u8]) -> Result<()> {
        let host = self.get_host_address(guest)?;
        let mut bytes_written = 0usize;
        let ok = unsafe {
            WriteProcessMemory(
                self.handle,
                host as *const c_void,
                bytes.as_ptr() as *const c_void,
                bytes.len(),
                &mut bytes_written,
            )
        };
        if ok == 0 || bytes_written != bytes.len() {
            let error = unsafe { GetLastError() };
            bail!(
                "WriteProcessMemory failed at host {host:#x} for {} bytes (wrote {bytes_written}, error {error})",
                bytes.len()
            );
        }
        Ok(())
    }

    pub fn read_u8(&mut self, guest: u64) -> Result<u8> {
        Ok(self.read_guest_bytes(guest, 1)?[0])
    }

    pub fn read_i8(&mut self, guest: u64) -> Result<i8> {
        Ok(self.read_u8(guest)? as i8)
    }

    pub fn read_u16(&mut self, guest: u64) -> Result<u16> {
        let bytes = self.read_guest_bytes(guest, 2)?;
        Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_i16(&mut self, guest: u64) -> Result<i16> {
        let bytes = self.read_guest_bytes(guest, 2)?;
        Ok(i16::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_u32(&mut self, guest: u64) -> Result<u32> {
        let bytes = self.read_guest_bytes(guest, 4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_i32(&mut self, guest: u64) -> Result<i32> {
        let bytes = self.read_guest_bytes(guest, 4)?;
        Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    #[allow(dead_code)]
    pub fn read_u64(&mut self, guest: u64) -> Result<u64> {
        let bytes = self.read_guest_bytes(guest, 8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_f32(&mut self, guest: u64) -> Result<f32> {
        let bytes = self.read_guest_bytes(guest, 4)?;
        Ok(f32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn read_bytes(&mut self, guest: u64, len: usize) -> Result<Vec<u8>> {
        self.read_guest_bytes(guest, len)
    }

    pub fn read_string(&mut self, guest: u64, len: usize) -> Result<String> {
        let bytes = self.read_guest_bytes(guest, len)?;
        let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[..end]).to_string())
    }

    pub fn read_wchar(&mut self, guest: u64, len: usize) -> Result<String> {
        let bytes = self.read_guest_bytes(guest, len)?;
        let mut words = Vec::new();
        for chunk in bytes.chunks_exact(2) {
            let word = u16::from_le_bytes([chunk[0], chunk[1]]);
            if word == 0 {
                break;
            }
            words.push(word);
        }
        Ok(String::from_utf16_lossy(&words))
    }

    pub fn formatted_bytes(&mut self, guest: u64, len: usize, columns: usize) -> Result<Vec<String>> {
        let bytes = self.read_guest_bytes(guest, len)?;
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let width = 3 * columns;
        let mut rows = Vec::new();
        let mut start = 0;
        while start < hex.len() {
            let end = (start + width).min(hex.len());
            rows.push(hex[start..end].trim().to_string());
            start += width;
        }
        Ok(rows)
    }

    #[allow(dead_code)]
    pub fn host_address_debug(&mut self, guest: u64) -> Result<String> {
        let host = self.get_host_address(guest)?;
        Ok(format!("{:#x} -> {:#x}", guest, host))
    }

    pub fn known_host_address(&self, guest: u64) -> Result<u64> {
        self.known_addresses
            .get(&guest)
            .copied()
            .ok_or_else(|| anyhow!("guest address {guest:#x} has not been translated"))
    }
}
