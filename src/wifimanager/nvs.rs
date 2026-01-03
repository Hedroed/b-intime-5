use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions;
use esp_storage::FlashStorage;

static mut NVS_READ_BUF: &mut [u8; 1024] = &mut [0; 1024];

pub struct Nvs {
    offset: usize,
    size: usize,
}

impl Nvs {
    pub fn new(
        flash: esp_hal::peripherals::FLASH<'static>,
        flash_offset: usize,
        flash_size: usize,
    ) -> Self {

        let mut flash = FlashStorage::new(flash); // peripherals.FLASH
        esp_println::println!("Flash size = {}", flash.capacity());

        let mut pt_mem = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
        let pt = partitions::read_partition_table(&mut flash, &mut pt_mem).unwrap();

        for i in 0..pt.len() {
            let raw = pt.get_partition(i).unwrap();
            esp_println::println!("{:?}", raw);
        }

        let nvs = pt
        .find_partition(partitions::PartitionType::Data(
            partitions::DataPartitionSubType::Nvs,
        ))
        .unwrap()
        .unwrap();
        let mut nvs_partition = nvs.as_embedded_storage(&mut flash);

        let mut bytes = [0u8; 1024];
        esp_println::println!("NVS partition size = {}", nvs_partition.capacity());

        let offset_in_nvs_partition = 0;

        nvs_partition
            .read(offset_in_nvs_partition, &mut bytes)
            .unwrap();
        esp_println::println!(
            "Read from {:x}:  {:02x?}",
            offset_in_nvs_partition,
            &bytes[..32]
        );

        Nvs {
            offset: 0,
            size: 32,
        }
    }

    pub async fn append_key(&self, key: &[u8], buf: &[u8]) -> Result<(), ()> {
        Ok(())
    }

    /// # Safety
    ///
    /// This doesn't check for semaphore!
    pub unsafe fn get_key_unchecked(&self, key: &[u8], buf: &mut [u8]) -> Result<(), ()> {
        Ok(())
    }

}
