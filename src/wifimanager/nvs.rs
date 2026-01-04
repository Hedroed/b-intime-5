use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions;
use esp_storage::FlashStorage;

use crate::wifimanager::structs::AutoSetupSettings;

pub struct Nvs {
    offset: u32,
    size: usize,
    region: partitions::FlashRegion<'static, FlashStorage<'static>>,
}

impl Nvs {
    pub fn new(
        flash: esp_hal::peripherals::FLASH<'static>,
        flash_offset: u32,
        flash_size: usize,
    ) -> crate::wifimanager::structs::Result<Self> {

        let flash = crate::mk_static!(FlashStorage<'static>, FlashStorage::new(flash)); // peripherals.FLASH
        esp_println::println!("Flash size = {}", flash.capacity());

        let pt_mem = crate::mk_static!([u8; partitions::PARTITION_TABLE_MAX_LEN], [0u8; partitions::PARTITION_TABLE_MAX_LEN]);
        let pt = partitions::read_partition_table(flash, pt_mem).unwrap();

        for i in 0..pt.len() {
            let raw = pt.get_partition(i).unwrap();
            esp_println::println!("{:?}", raw);
        }

        let nvs = pt
        .find_partition(partitions::PartitionType::Data(
            partitions::DataPartitionSubType::Nvs,
        ))?
        .unwrap();

        let nvs_partition = nvs.as_embedded_storage(flash);
        esp_println::println!("NVS partition size = {}", nvs_partition.capacity());

        Ok(Nvs {
            offset: flash_offset,
            size: flash_size,
            region: nvs_partition,
        })
    }

    pub async fn write(&mut self, buf: &[u8]) -> crate::wifimanager::structs::Result<()> {
        self.region
            .write(self.offset, &buf[..self.size])?;
        Ok(())
    }

    pub fn read(&mut self, buf: &mut [u8]) -> crate::wifimanager::structs::Result<()> {

        self.region
            .read(self.offset, buf)?;

        esp_println::println!(
            "Read from {:x}:  {:02x?}",
            self.offset,
            &buf[..self.size]
        );
        Ok(())
    }

}


pub struct SavedSettings {
    nvs: Nvs,
    buf: [u8; 1024],
}

impl SavedSettings {
    pub fn new(
        flash: esp_hal::peripherals::FLASH<'static>,
    ) -> crate::wifimanager::structs::Result<Self> {
        Ok(Self {
            nvs: Nvs::new(flash, 0, 1024)?,
            buf: [0u8; 1024],
        })
    }

    pub fn load(&mut self) -> crate::wifimanager::structs::Result<AutoSetupSettings> {
        let _ = self.nvs.read(&mut self.buf);

        let end_pos = self.buf
                .iter()
                .position(|&x| x == 0x00)
                .unwrap_or(self.buf.len());

        Ok(
            serde_json_core::from_slice::<AutoSetupSettings>(
                &self.buf[..end_pos],
            )?.0
        )
    }

    pub fn save(&mut self, settings: &AutoSetupSettings) -> crate::wifimanager::structs::Result<()> {
        self.buf.fill(0u8);

        serde_json_core::to_slice(
            settings,
            &mut self.buf,
        )?;

        Ok(())
    }
}