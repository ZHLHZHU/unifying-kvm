cmake_minimum_required(VERSION 3.13)
if(NOT PICO_SDK_PATH)
    set(PICO_SDK_PATH "$ENV{PICO_SDK_PATH}")
endif()
if(NOT EXISTS "${PICO_SDK_PATH}/pico_sdk_init.cmake")
    message(FATAL_ERROR "Set PICO_SDK_PATH to Pico SDK 2.2.0 (a1438dff1d38bd9c65dbd693f0e5db4b9ae91779)")
endif()
include(${PICO_SDK_PATH}/external/pico_sdk_import.cmake)
project(rp2040_kvm C CXX ASM)
set(CMAKE_C_STANDARD 11)
pico_sdk_init()
execute_process(COMMAND git -C "${PICO_SDK_PATH}" rev-parse HEAD OUTPUT_VARIABLE SDK_COMMIT OUTPUT_STRIP_TRAILING_WHITESPACE COMMAND_ERROR_IS_FATAL ANY)
if(NOT SDK_COMMIT STREQUAL "a1438dff1d38bd9c65dbd693f0e5db4b9ae91779")
    message(FATAL_ERROR "Unsupported Pico SDK revision: ${SDK_COMMIT}")
endif()
execute_process(COMMAND git -C "${PICO_TINYUSB_PATH}" rev-parse HEAD OUTPUT_VARIABLE TINYUSB_COMMIT OUTPUT_STRIP_TRAILING_WHITESPACE COMMAND_ERROR_IS_FATAL ANY)
if(NOT TINYUSB_COMMIT STREQUAL "86ad6e56c1700e85f1c5678607a762cfe3aa2f47")
    message(FATAL_ERROR "TinyUSB must match the SDK submodule revision")
endif()
# Replace only the native HCD, keeping the pinned SDK and shared USB support.
# RP2040's asynchronous endpoints cannot recover from a repeated IN data PID.
get_target_property(TINYUSB_HOST_SOURCES tinyusb_host_base INTERFACE_SOURCES)
list(REMOVE_ITEM TINYUSB_HOST_SOURCES "${PICO_TINYUSB_PATH}/src/portable/raspberrypi/rp2040/hcd_rp2040.c")
set_property(TARGET tinyusb_host_base PROPERTY INTERFACE_SOURCES "${TINYUSB_HOST_SOURCES}")
find_program(CARGO_EXECUTABLE cargo REQUIRED)
option(RP_UART_SMOKE "Run periodic UART peer diagnostics instead of the KVM runtime" OFF)
# Production keeps its original Cargo command and artifact location.
if(RP_UART_SMOKE)
    set(RUST_TARGET_DIR "${CMAKE_BINARY_DIR}/cargo-uart-smoke")
    set(RUST_ARCHIVE "${RUST_TARGET_DIR}/thumbv6m-none-eabi/release/librp2040_kvm.a")
    add_custom_target(rp2040_kvm_rust
        COMMAND ${CMAKE_COMMAND} -E env "CARGO_TARGET_DIR=${RUST_TARGET_DIR}"
            ${CARGO_EXECUTABLE} build --manifest-path "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
            --release --target thumbv6m-none-eabi --features uart-smoke
        BYPRODUCTS "${RUST_ARCHIVE}"
        WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
        VERBATIM)
else()
    set(RUST_ARCHIVE "${CMAKE_CURRENT_SOURCE_DIR}/target/thumbv6m-none-eabi/release/librp2040_kvm.a")
    # Always ask Cargo to check its own dependencies; Cargo performs incremental builds.
    add_custom_target(rp2040_kvm_rust
        COMMAND ${CARGO_EXECUTABLE} build --manifest-path "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml" --release --target thumbv6m-none-eabi
        BYPRODUCTS "${RUST_ARCHIVE}"
        WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
        VERBATIM)
endif()
add_library(rp2040_kvm_core STATIC IMPORTED GLOBAL)
set_target_properties(rp2040_kvm_core PROPERTIES IMPORTED_LOCATION "${RUST_ARCHIVE}")
add_dependencies(rp2040_kvm_core rp2040_kvm_rust)
add_executable(rp2040_kvm native/main.c native/bridge.c native/uart_ota_relay.c native/hcd_epx.c)
if(RP_UART_SMOKE)
    target_compile_definitions(rp2040_kvm PRIVATE RP_UART_SMOKE=1)
endif()
add_dependencies(rp2040_kvm rp2040_kvm_rust)
target_include_directories(rp2040_kvm PRIVATE ${CMAKE_CURRENT_SOURCE_DIR} ${CMAKE_CURRENT_SOURCE_DIR}/native
    "${PICO_TINYUSB_PATH}/src/portable/raspberrypi/rp2040")
target_link_libraries(rp2040_kvm PRIVATE rp2040_kvm_core pico_stdlib tinyusb_host tinyusb_board hardware_uart hardware_irq hardware_watchdog)
pico_enable_stdio_uart(rp2040_kvm 1)
pico_enable_stdio_usb(rp2040_kvm 0)
pico_set_program_name(rp2040_kvm "rp2040-kvm")
pico_set_program_version(rp2040_kvm "1")
pico_add_extra_outputs(rp2040_kvm)
target_compile_definitions(rp2040_kvm PRIVATE PICO_STACK_SIZE=16384)
pico_set_linker_script(rp2040_kvm "${CMAKE_CURRENT_SOURCE_DIR}/memmap.ld")
