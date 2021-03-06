#![feature(alloc)]
#![feature(panic_implementation)]
#![feature(alloc_error_handler)]
#![no_main]
#![no_std]

#[macro_use]
extern crate alloc;
extern crate cortex_m;
#[macro_use]
extern crate cortex_m_rt as rt;
extern crate alloc_cortex_m;
extern crate cortex_m_semihosting as sh;
#[macro_use]
extern crate stm32f7;
#[macro_use]
extern crate stm32f7_discovery;
extern crate smoltcp;

use alloc::vec::Vec;
use alloc_cortex_m::CortexMHeap;
use core::alloc::Layout as AllocLayout;
use core::fmt::Write;
use core::panic::PanicInfo;
use cortex_m::{asm, interrupt};
use rt::ExceptionFrame;
use sh::hio::{self, HStdout};
use smoltcp::{
    socket::{
        Socket, SocketSet, TcpSocket, TcpSocketBuffer, UdpPacketMetadata, UdpSocket,
        UdpSocketBuffer,
    },
    time::Instant,
    wire::{EthernetAddress, IpAddress, IpEndpoint, Ipv4Address},
};
use stm32f7::stm32f7x6::{CorePeripherals, Interrupt, Peripherals};
use stm32f7_discovery::{
    ethernet,
    gpio::{GpioPort, InputPin, OutputPin},
    init,
    lcd::{self, Color},
    random::Rng,
    sd,
    system_clock::{self, Hz},
    touch,
};

#[global_allocator]
static ALLOCATOR: CortexMHeap = CortexMHeap::empty();

const HEAP_SIZE: usize = 50 * 1024; // in bytes
const ETH_ADDR: EthernetAddress = EthernetAddress([0x00, 0x08, 0xdc, 0xab, 0xcd, 0xef]);
const IP_ADDR: Ipv4Address = Ipv4Address([141, 52, 46, 198]);

entry!(main);

fn main() -> ! {
    let core_peripherals = CorePeripherals::take().unwrap();
    let mut systick = core_peripherals.SYST;
    let mut nvic = core_peripherals.NVIC;

    let peripherals = Peripherals::take().unwrap();
    let mut rcc = peripherals.RCC;
    let mut pwr = peripherals.PWR;
    let mut flash = peripherals.FLASH;
    let mut fmc = peripherals.FMC;
    let mut ltdc = peripherals.LTDC;
    let mut sai_2 = peripherals.SAI2;
    let mut rng = peripherals.RNG;
    let mut sdmmc = peripherals.SDMMC1;
    let mut syscfg = peripherals.SYSCFG;
    let mut ethernet_mac = peripherals.ETHERNET_MAC;
    let mut ethernet_dma = peripherals.ETHERNET_DMA;

    init::init_system_clock_216mhz(&mut rcc, &mut pwr, &mut flash);
    init::enable_gpio_ports(&mut rcc);

    let gpio_a = GpioPort::new_a(&peripherals.GPIOA);
    let gpio_b = GpioPort::new_b(&peripherals.GPIOB);
    let gpio_c = GpioPort::new(&peripherals.GPIOC);
    let gpio_d = GpioPort::new(&peripherals.GPIOD);
    let gpio_e = GpioPort::new(&peripherals.GPIOE);
    let gpio_f = GpioPort::new(&peripherals.GPIOF);
    let gpio_g = GpioPort::new(&peripherals.GPIOG);
    let gpio_h = GpioPort::new(&peripherals.GPIOH);
    let gpio_i = GpioPort::new(&peripherals.GPIOI);
    let gpio_j = GpioPort::new(&peripherals.GPIOJ);
    let gpio_k = GpioPort::new(&peripherals.GPIOK);
    let mut pins = init::pins(
        gpio_a, gpio_b, gpio_c, gpio_d, gpio_e, gpio_f, gpio_g, gpio_h, gpio_i, gpio_j, gpio_k,
    );

    // configures the system timer to trigger a SysTick exception every second
    init::init_systick(Hz(100), &mut systick, &rcc);
    systick.enable_interrupt();

    init::init_sdram(&mut rcc, &mut fmc);
    let mut lcd = init::init_lcd(&mut ltdc, &mut rcc);
    pins.display_enable.set(true);
    pins.backlight.set(true);

    let mut layer_1 = lcd.layer_1().unwrap();
    let mut layer_2 = lcd.layer_2().unwrap();

    layer_1.clear();
    let mut audio_writer = layer_1.audio_writer();
    layer_2.clear();
    lcd::init_stdout(layer_2);

    println!("Hello World");

    // Initialize the allocator BEFORE you use it
    unsafe { ALLOCATOR.init(rt::heap_start() as usize, HEAP_SIZE) }

    let xs = vec![1, 2, 3];

    let mut i2c_3 = init::init_i2c_3(&peripherals.I2C3, &mut rcc);
    i2c_3.test_1();
    i2c_3.test_2();

    nvic.enable(Interrupt::EXTI0);

    let mut sd = sd::Sd::new(&mut sdmmc, &mut rcc, &pins.sdcard_present);

    init::init_sai_2(&mut sai_2, &mut rcc);
    init::init_wm8994(&mut i2c_3).expect("WM8994 init failed");
    // touch initialization should be done after audio initialization, because the touch
    // controller might not be ready yet
    touch::check_family_id(&mut i2c_3).unwrap();

    let mut rng = Rng::init(&mut rng, &mut rcc).expect("RNG init failed");
    print!("Random numbers: ");
    for _ in 0..4 {
        print!(
            "{} ",
            rng.poll_and_get()
                .expect("Failed to generate random number")
        );
    }
    println!("");

    // ethernet
    let mut ethernet_interface = ethernet::EthernetDevice::new(
        Default::default(),
        Default::default(),
        &mut rcc,
        &mut syscfg,
        &mut ethernet_mac,
        &mut ethernet_dma,
        ETH_ADDR,
    ).map(|device| device.into_interface(IP_ADDR));
    if let Err(e) = ethernet_interface {
        println!("ethernet init failed: {:?}", e);
    };

    let mut sockets = SocketSet::new(Vec::new());

    if ethernet_interface.is_ok() {
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(IP_ADDR), 15);
        let udp_rx_buffer = UdpSocketBuffer::new(vec![UdpPacketMetadata::EMPTY; 3], vec![0u8; 256]);
        let udp_tx_buffer = UdpSocketBuffer::new(vec![UdpPacketMetadata::EMPTY; 1], vec![0u8; 128]);
        let mut example_udp_socket = UdpSocket::new(udp_rx_buffer, udp_tx_buffer);
        example_udp_socket.bind(endpoint).unwrap();
        sockets.add(example_udp_socket);

        let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; ethernet::MTU]);
        let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; ethernet::MTU]);
        let mut example_tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
        example_tcp_socket.listen(endpoint).unwrap();
        sockets.add(example_tcp_socket);
    }

    let mut previous_button_state = pins.button.get();
    loop {
        // poll button state
        let current_button_state = pins.button.get();
        if current_button_state != previous_button_state {
            if current_button_state {
                pins.led.toggle();

                // trigger the `EXTI0` interrupt
                nvic.set_pending(Interrupt::EXTI0);
            }

            previous_button_state = current_button_state;
        }

        // poll for new touch data
        for touch in &touch::touches(&mut i2c_3).unwrap() {
            audio_writer.layer().print_point_color_at(
                touch.x as usize,
                touch.y as usize,
                Color::from_hex(0xffff00),
            );
        }

        // poll for new audio data
        while sai_2.bsr.read().freq().bit_is_clear() {} // fifo_request_flag
        let data0 = sai_2.bdr.read().data().bits();
        while sai_2.bsr.read().freq().bit_is_clear() {} // fifo_request_flag
        let data1 = sai_2.bdr.read().data().bits();

        audio_writer.set_next_col(data0, data1);

        // handle new ethernet packets
        if let Ok(ref mut eth) = ethernet_interface {
            match eth.poll(
                &mut sockets,
                Instant::from_millis(system_clock::ms() as i64),
            ) {
                Err(::smoltcp::Error::Exhausted) => continue,
                Err(::smoltcp::Error::Unrecognized) => {}
                Err(e) => println!("Network error: {:?}", e),
                Ok(socket_changed) => if socket_changed {
                    for mut socket in sockets.iter_mut() {
                        poll_socket(&mut socket).expect("socket poll failed");
                    }
                },
            }
        }

        // Initialize the SD Card on insert and deinitialize on extract.
        if sd.card_present() && !sd.card_initialized() {
            if let Some(i_err) = sd::init(&mut sd).err() {
                println!("{:?}", i_err);
            }
        } else if !sd.card_present() && sd.card_initialized() {
            sd::de_init(&mut sd);
        }
    }
}

fn poll_socket(socket: &mut Socket) -> Result<(), smoltcp::Error> {
    match socket {
        &mut Socket::Udp(ref mut socket) => match socket.endpoint().port {
            15 => loop {
                let reply;
                match socket.recv() {
                    Ok((data, remote_endpoint)) => {
                        let mut data = Vec::from(data);
                        let len = data.len() - 1;
                        data[..len].reverse();
                        reply = (data, remote_endpoint);
                    }
                    Err(smoltcp::Error::Exhausted) => break,
                    Err(err) => return Err(err),
                }
                socket.send_slice(&reply.0, reply.1)?;
            },
            _ => {}
        },
        &mut Socket::Tcp(ref mut socket) => match socket.local_endpoint().port {
            15 => {
                if !socket.may_recv() {
                    return Ok(());
                }
                let reply = socket.recv(|data| {
                    if data.len() > 0 {
                        let mut reply = Vec::from("tcp: ");
                        let start_index = reply.len();
                        reply.extend_from_slice(data);
                        reply[start_index..(start_index + data.len() - 1)].reverse();
                        (data.len(), Some(reply))
                    } else {
                        (data.len(), None)
                    }
                })?;
                if let Some(reply) = reply {
                    assert_eq!(socket.send_slice(&reply)?, reply.len());
                }
            }
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

interrupt!(EXTI0, exti0, state: Option<HStdout> = None);

fn exti0(_state: &mut Option<HStdout>) {
    println!("Interrupt fired! This means that the button was pressed.");
}

exception!(SysTick, sys_tick, state: Option<HStdout> = None);

fn sys_tick(_state: &mut Option<HStdout>) {
    system_clock::tick();
    // print a `.` every 500ms
    if system_clock::ticks() % 50 == 0 && lcd::stdout::is_initialized() {
        print!(".");
    }
}

exception!(HardFault, hard_fault);

fn hard_fault(ef: &ExceptionFrame) -> ! {
    panic!("HardFault at {:#?}", ef);
}

exception!(*, default_handler);

fn default_handler(irqn: i16) {
    panic!("Unhandled exception (IRQn = {})", irqn);
}

// define what happens in an Out Of Memory (OOM) condition
#[alloc_error_handler]
fn rust_oom(_: AllocLayout) -> ! {
    panic!("out of memory");
}

#[panic_implementation]
fn panic(info: &PanicInfo) -> ! {
    interrupt::disable();

    if lcd::stdout::is_initialized() {
        println!("{}", info);
    }

    if let Ok(mut hstdout) = hio::hstdout() {
        let _ = writeln!(hstdout, "{}", info);
    }

    // OK to fire a breakpoint here because we know the microcontroller is connected to a debugger
    asm::bkpt();

    loop {}
}
