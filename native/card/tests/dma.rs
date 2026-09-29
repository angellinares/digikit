use emmc_card::{
    Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead,
    dma::{StorageDmaError, service_dma59},
};
use periph::{
    edma::{self, TcdView},
    esdhc::{self, DmaDirection, DmaError, Esdhc},
    regfile::RegFile,
};

struct Bytes(Vec<u8>);
impl RandomAccessRead for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&self, offset: u64, dst: &mut [u8]) -> usize {
        let source = self.0.get(offset as usize..).unwrap_or_default();
        let n = source.len().min(dst.len());
        dst[..n].copy_from_slice(&source[..n]);
        n
    }
}

fn configure(regs: &mut RegFile, count: u16, nbytes: u32, addr: u32) {
    let t = edma::TCD_BASE + (esdhc::DMA_CHANNEL * 0x20) as u32;
    regs.write(t + edma::SADDR as u32, 4, addr);
    regs.write(t + edma::SOFF as u32, 2, nbytes);
    regs.write(
        t + edma::SLAST as u32,
        4,
        (-(count as i32 * nbytes as i32)) as u32,
    );
    regs.write(t + edma::DADDR as u32, 4, addr);
    regs.write(t + edma::NBYTES as u32, 4, nbytes);
    regs.write(t + edma::CITER as u32, 2, count as u32);
    regs.write(t + edma::BITER as u32, 2, count as u32);
    regs.write(t + edma::CSR as u32, 2, edma::CSR_INT_MAJOR as u32);
}

#[test]
fn backed_cmd18_and_overlay_cmd25_commit_guest_card_and_tcd() {
    let base: Vec<_> = (0..1024).map(|n| (n % 251) as u8).collect();
    let card =
        Card::with_backing(DEFAULT_CAPACITY_BLOCKS, Some(Box::new(Bytes(base.clone())))).unwrap();
    let mut controller = Esdhc::new(card);
    let mut regs = RegFile::new();
    configure(&mut regs, 1, 512, 0x8000_0000);
    let mut guest = [0u8; 512];
    let mut scratch = [0u8; 512];
    let read = service_dma59(
        controller.card_mut(),
        &mut regs,
        18,
        1,
        0x8000_0000,
        &mut guest,
        &mut scratch,
    )
    .unwrap();
    assert_eq!(read.direction, DmaDirection::CardToGuest);
    assert_eq!(guest.as_slice(), &base[512..]);
    let tcd = TcdView::new(&mut regs, esdhc::DMA_CHANNEL).snapshot();
    assert_eq!(
        (tcd.daddr, tcd.citer, tcd.csr & edma::CSR_DONE),
        (0x8000_0200, 1, edma::CSR_DONE)
    );
    assert!(read.completion.major_interrupt);

    configure(&mut regs, 1, 512, 0x8000_0000);
    guest[0] = 0x5a;
    let written = service_dma59(
        controller.card_mut(),
        &mut regs,
        25,
        1,
        0x8000_0000,
        &mut guest,
        &mut scratch,
    )
    .unwrap();
    assert_eq!(written.direction, DmaDirection::GuestToCard);
    assert_eq!(
        TcdView::new(&mut regs, esdhc::DMA_CHANNEL).snapshot().saddr,
        0x8000_0000
    );
    assert_eq!(
        controller
            .card_mut()
            .data_for(18, 1, 512)
            .unwrap()
            .unwrap()
            .as_slice(),
        guest.as_slice()
    );
}

#[test]
fn failed_request_keeps_card_guest_and_tcd_unchanged() {
    let mut card = Card::default();
    let mut regs = RegFile::new();
    configure(&mut regs, 1, 512, 0x8000_0000);
    let original = *regs.raw();
    let mut guest = [0xa5u8; 512];
    let mut short = [0u8; 511];
    assert_eq!(
        service_dma59(
            &mut card,
            &mut regs,
            25,
            1,
            0x8000_0000,
            &mut guest,
            &mut short
        ),
        Err(StorageDmaError::ScratchTooSmall {
            needed: 512,
            available: 511
        })
    );
    assert_eq!(card.overlay_len(), 0);
    assert_eq!(guest, [0xa5; 512]);
    assert_eq!(*regs.raw(), original);

    let mut scratch = [0u8; 512];
    assert_eq!(
        service_dma59(
            &mut card,
            &mut regs,
            25,
            1,
            0x8000_1000,
            &mut guest,
            &mut scratch
        ),
        Err(StorageDmaError::Dma(DmaError::GuestOutOfRange {
            address: 0x8000_0000,
            bytes: 512
        }))
    );
    assert_eq!(*regs.raw(), original);
    assert_eq!(card.overlay_len(), 0);
}

#[test]
fn cmd8_ext_csd_and_zero_citer_follow_oracle() {
    let mut card = Card::default();
    let mut regs = RegFile::new();
    configure(&mut regs, 1, 512, 0x8000_0000);
    let mut guest = [0u8; 512];
    let mut scratch = [0u8; 512];
    service_dma59(
        &mut card,
        &mut regs,
        8,
        0,
        0x8000_0000,
        &mut guest,
        &mut scratch,
    )
    .unwrap();
    assert_eq!(guest[0xd4..0xd8], [0x00, 0x76, 0, 0]);

    configure(&mut regs, 0, 512, 0x8000_0000);
    let before = *regs.raw();
    let result = service_dma59(
        &mut card,
        &mut regs,
        18,
        0,
        0x8000_0000,
        &mut guest,
        &mut scratch,
    )
    .unwrap();
    assert!(!result.completion.done);
    assert_eq!(*regs.raw(), before);
}
