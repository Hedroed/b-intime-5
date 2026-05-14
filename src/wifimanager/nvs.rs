use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions;
use esp_storage::FlashStorage;

use super::structs::AutoSetupSettings;

pub struct Nvs {
    offset: u32,
    size: usize,
    region: partitions::FlashRegion<'static, FlashStorage<'static>>,
}

impl Nvs {
    pub fn new(
        flash: esp_hal::peripherals::FLASH<'static>,
        flash_size: usize,
    ) -> super::structs::Result<Self> {
        let flash = crate::mk_static!(FlashStorage<'static>, FlashStorage::new(flash)); // peripherals.FLASH
        defmt::info!("Flash size = {}", flash.capacity());

        let pt_mem = crate::mk_static!(
            [u8; partitions::PARTITION_TABLE_MAX_LEN],
            [0u8; partitions::PARTITION_TABLE_MAX_LEN]
        );
        let pt = partitions::read_partition_table(flash, pt_mem)
            .map_err(|_| super::structs::WmError::NvsError)?;

        // for i in 0..pt.len() {
        //     if let Ok(raw) = pt.get_partition(i) {
        //         defmt::info!("{}", defmt::Debug2Format(&raw));
        //     }
        // }

        let nvs = pt
            .find_partition(partitions::PartitionType::Data(
                partitions::DataPartitionSubType::Nvs,
            ))?
            .ok_or(super::structs::WmError::NvsError)?;

        let nvs_partition = nvs.as_embedded_storage(flash);
        defmt::info!("NVS partition size = {}", nvs_partition.capacity());

        Ok(Nvs {
            offset: 0,
            size: flash_size,
            region: nvs_partition,
        })
    }

    pub fn write(&mut self, buf: &[u8]) -> super::structs::Result<()> {
        self.region.write(self.offset, &buf[..self.size])?;
        Ok(())
    }

    pub fn read(&mut self, buf: &mut [u8]) -> super::structs::Result<()> {
        self.region.read(self.offset, buf)?;
        Ok(())
    }
}

pub struct SavedSettings {
    nvs: Nvs,
    buf: [u8; 1024],
}

impl SavedSettings {
    pub fn new(flash: esp_hal::peripherals::FLASH<'static>) -> super::structs::Result<Self> {
        Ok(Self {
            nvs: Nvs::new(flash, 1024)?,
            buf: [0u8; 1024],
        })
    }

    pub fn load(&mut self) -> super::structs::Result<Option<AutoSetupSettings>> {
        let _ = self.nvs.read(&mut self.buf);

        let end_pos = self
            .buf
            .iter()
            .position(|&x| x == 0x00)
            .unwrap_or(self.buf.len());

        if let Ok((data, _)) =
            serde_json_core::from_slice::<AutoSetupSettings>(&self.buf[..end_pos])
        {
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    pub fn save(&mut self, settings: &AutoSetupSettings) -> super::structs::Result<()> {
        self.buf.fill(0u8);

        serde_json_core::to_slice(settings, &mut self.buf)?;
        defmt::info!("write to nvs: {=[u8]}", self.buf);

        self.nvs.write(&self.buf)?;

        Ok(())
    }
}
