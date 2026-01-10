use std::collections::HashMap;
use std::sync::Arc;
use anyhow::{Result, anyhow};
use windows::Win32::Foundation::{HANDLE, CloseHandle, FALSE};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_VM_READ, PROCESS_QUERY_INFORMATION};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use std::ffi::c_void;
use crate::qmp::QmpClient;

pub struct MemoryReader {
    process_handle: HANDLE,
    qmp: Arc<QmpClient>,
    // known_addresses maps Guest Physical Address (GPA) to Host Virtual Address (HVA)
    known_addresses: HashMap<u32, u64>, 
    // memory_cache stores chunks of memory: (start_gpa, end_gpa) -> bytes
    memory_cache: HashMap<(u32, u32), Vec<u8>>,
    // Cache for GVA -> HVA lookups (Host Address) to avoid repeated map lookups if possible, 
    // but mainly we just use known_addresses.
}

// Ensure HANDLE is Send (it is just a pointer/integer, effectively)
unsafe impl Send for MemoryReader {}

impl MemoryReader {
    pub fn new(pid: u32, qmp: Arc<QmpClient>) -> Result<Self> {
        unsafe {
            let handle = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, FALSE, pid)?;
            if handle.is_invalid() {
                return Err(anyhow!("Failed to open process with PID {}", pid));
            }
            Ok(Self {
                process_handle: handle,
                qmp,
                known_addresses: HashMap::new(),
                memory_cache: HashMap::new(),
            })
        }
    }

    pub fn invalidate_cache(&mut self) {
        self.memory_cache.clear();
    }

    pub async fn add_to_cache(&mut self, addr: u32, size: u32) -> Result<()> {
        let hva = self.get_host_address(addr).await?;
        let data = self.read_process_memory(hva, size as usize)?;
        self.memory_cache.insert((addr, addr + size), data);
        Ok(())
    }

    fn read_from_cache(&self, addr: u32, size: usize) -> Option<Vec<u8>> {
        for ((start, end), data) in &self.memory_cache {
            if addr >= *start && (addr + size as u32) <= *end {
                let offset = (addr - start) as usize;
                if offset + size <= data.len() {
                    return Some(data[offset..offset + size].to_vec());
                }
            }
        }
        None
    }

    async fn get_host_address(&mut self, addr: u32) -> Result<u64> {
        if let Some(&hva) = self.known_addresses.get(&addr) {
            return Ok(hva);
        }

        // Check if it's in a cached block to calculate HVA? 
        // Logic: if we cached a block at GPA X, and we know HVA for X, we can calc HVA for X+offset.
        // But for now, let's just rely on explicit translation if missing.
        
        // Optimistic assumption for contiguous RAM > 0x80000000 (from Python script)
        if addr > 0x80000000 {
            if let Some(&base_hva) = self.known_addresses.get(&0x80000000) {
                 let offset = addr - 0x80000000;
                 return Ok(base_hva + offset as u64);
            }
        }

        let hva = self.qmp.gva2hva(addr).await?;
        self.known_addresses.insert(addr, hva);
        Ok(hva)
    }

    // Helper to read raw bytes from process memory (HVA)
    fn read_process_memory(&self, hva: u64, size: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        let mut bytes_read = 0;
        unsafe {
            ReadProcessMemory(
                self.process_handle,
                hva as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                size,
                Some(&mut bytes_read),
            )?;
        }
        if bytes_read != size {
            return Err(anyhow!("ReadProcessMemory incomplete: requested {}, got {}", size, bytes_read));
        }
        Ok(buffer)
    }
    
    pub async fn read_bytes(&mut self, addr: u32, size: usize) -> Result<Vec<u8>> {
        // 1. Check Cache
        if let Some(data) = self.read_from_cache(addr, size) {
            return Ok(data);
        }

        // 2. Get Host Address
        let hva = self.get_host_address(addr).await?;

        // 3. Read Process Memory
        self.read_process_memory(hva, size)
    }

    // Typed reads
    pub async fn read_u8(&mut self, addr: u32) -> Result<u8> {
        let bytes = self.read_bytes(addr, 1).await?;
        Ok(bytes[0])
    }

    pub async fn read_u32_uncached(&mut self, addr: u32) -> Result<u32> {
        let hva = self.get_host_address(addr).await?;
        let bytes = self.read_process_memory(hva, 4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub async fn read_u16(&mut self, addr: u32) -> Result<u16> {
        let bytes = self.read_bytes(addr, 2).await?;
        Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub async fn read_u32(&mut self, addr: u32) -> Result<u32> {
        let bytes = self.read_bytes(addr, 4).await?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub async fn read_s16(&mut self, addr: u32) -> Result<i16> {
        let bytes = self.read_bytes(addr, 2).await?;
        Ok(i16::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub async fn read_s32(&mut self, addr: u32) -> Result<i32> {
        let bytes = self.read_bytes(addr, 4).await?;
        Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub async fn read_f32(&mut self, addr: u32) -> Result<f32> {
        let bytes = self.read_bytes(addr, 4).await?;
        Ok(f32::from_le_bytes(bytes.try_into().unwrap()))
    }
    
    pub async fn read_string(&mut self, addr: u32, max_len: usize) -> Result<String> {
        // Read bytes, find null terminator
        let bytes = self.read_bytes(addr, max_len).await?;
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(max_len);
        Ok(String::from_utf8_lossy(&bytes[..len]).into_owned())
    }
    
    pub async fn read_wchar(&mut self, addr: u32, max_len: usize) -> Result<String> {
        // Read bytes (max_len * 2), decode utf16
        let bytes = self.read_bytes(addr, max_len * 2).await?;
        let u16s: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        
        let len = u16s.iter().position(|&c| c == 0).unwrap_or(u16s.len());
        Ok(String::from_utf16_lossy(&u16s[..len]))
    }
}

impl Drop for MemoryReader {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process_handle);
        }
    }
}
