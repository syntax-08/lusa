//
// Lusa repository model: ESP32-C3 general-purpose SPI2 controller (master).
//
// Renode 1.17 has no ESP32-C3 SPI model.  This one covers CPU-driven user
// transactions:
//
//   CMD +0x00 (UPDATE bit 23, USR bit 24), CTRL +0x08, CLOCK +0x0C,
//   USER +0x10, USER1 +0x14, USER2 +0x18, MS_DLEN +0x1C, MISC +0x20,
//   DMA_INT_ENA +0x34, DMA_INT_CLR +0x38, DMA_INT_RAW +0x3C,
//   DMA_INT_ST +0x40, W0..W15 +0x98..+0xD4, SLAVE +0xE0, CLK_GATE +0xE8.
//
// Setting CMD.USR clocks MS_DLEN + 1 bits (rounded up to bytes) through the
// attached ISPIPeripheral, least significant byte of W0 first, replaces the
// buffer with the replies, clears USR, and raises TRANS_DONE (bit 12).
// UPDATE clears itself at once.
//
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.SPI
{
    public class LusaEsp32C3Spi : NullRegistrationPointPeripheralContainer<ISPIPeripheral>, IDoubleWordPeripheral, IKnownSize
    {
        public LusaEsp32C3Spi(IMachine machine) : base(machine)
        {
            IRQ = new GPIO();
            Reset();
        }

        public override void Reset()
        {
            System.Array.Clear(registers, 0, registers.Length);
            System.Array.Clear(buffer, 0, buffer.Length);
            interruptEnable = 0;
            interruptRaw = 0;
            IRQ.Unset();
        }

        public uint ReadDoubleWord(long offset)
        {
            if(offset >= Buffer && offset < Buffer + 16 * 4)
            {
                return buffer[(offset - Buffer) / 4];
            }
            switch(offset)
            {
            case InterruptEnable:
                return interruptEnable;
            case InterruptRaw:
                return interruptRaw;
            case InterruptStatus:
                return interruptRaw & interruptEnable;
            }
            if(offset >= 0 && offset < Size && offset % 4 == 0)
            {
                return registers[offset / 4];
            }
            this.Log(LogLevel.Warning, "Unhandled read at 0x{0:X}", offset);
            return 0;
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            if(offset >= Buffer && offset < Buffer + 16 * 4)
            {
                buffer[(offset - Buffer) / 4] = value;
                return;
            }
            switch(offset)
            {
            case Command:
                if((value & User) != 0)
                {
                    Transfer();
                }
                // UPDATE and USR complete immediately.
                registers[0] = value & ~(Update | User);
                break;
            case InterruptEnable:
                interruptEnable = value;
                break;
            case InterruptClear:
                interruptRaw &= ~value;
                break;
            default:
                if(offset >= 0 && offset < Size && offset % 4 == 0)
                {
                    registers[offset / 4] = value;
                }
                else
                {
                    this.Log(LogLevel.Warning, "Unhandled write at 0x{0:X}: 0x{1:X}", offset, value);
                }
                break;
            }
            RefreshIrq();
        }

        public long Size => 0x100;

        public GPIO IRQ { get; }

        private void Transfer()
        {
            var bits = (registers[DataLength / 4] & 0x3FFFF) + 1;
            var bytes = (int)System.Math.Min((bits + 7) / 8, 64);
            for(var i = 0; i < bytes; i++)
            {
                var word = i / 4;
                var shift = (i % 4) * 8;
                var sent = (byte)(buffer[word] >> shift);
                var reply = RegisteredPeripheral?.Transmit(sent) ?? (byte)0;
                buffer[word] = (buffer[word] & ~(0xFFu << shift)) | ((uint)reply << shift);
            }
            RegisteredPeripheral?.FinishTransmission();
            interruptRaw |= TransDone;
        }

        private void RefreshIrq()
        {
            IRQ.Set((interruptRaw & interruptEnable) != 0);
        }

        private uint interruptEnable;
        private uint interruptRaw;
        private readonly uint[] registers = new uint[0x100 / 4];
        private readonly uint[] buffer = new uint[16];

        private const long Command = 0x00;
        private const long DataLength = 0x1C;
        private const long InterruptEnable = 0x34;
        private const long InterruptClear = 0x38;
        private const long InterruptRaw = 0x3C;
        private const long InterruptStatus = 0x40;
        private const long Buffer = 0x98;
        private const uint Update = 1u << 23;
        private const uint User = 1u << 24;
        private const uint TransDone = 1u << 12;
    }
}
