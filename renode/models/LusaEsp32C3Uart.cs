//
// Lusa repository model: ESP32-C3 UART with FIFO interrupts.
//
// Renode 1.17's ESP32_UART never raises its receive interrupt and reports no
// FIFO counts, so interrupt-driven drivers cannot run on it.  This model keeps
// the same register map for the parts Lusa uses:
//
//   FIFO +0x00, INT_RAW +0x04, INT_ST +0x08, INT_ENA +0x0C, INT_CLR +0x10,
//   CLKDIV +0x14, STATUS +0x1C (RXFIFO_CNT 9:0, TXFIFO_CNT 25:16),
//   CONF0 +0x20, CONF1 +0x24 (RXFIFO_FULL_THRHD 8:0, TXFIFO_EMPTY_THRHD 17:9),
//   CLK_CONF +0x78.
//
// RXFIFO_FULL (bit 0) is raw while the receive FIFO holds at least the
// threshold; TXFIFO_EMPTY (bit 1) while the transmit FIFO holds fewer bytes
// than its threshold.  Transmission is immediate, so the transmit FIFO is
// always empty.  Both are level conditions: INT_CLR only clears them once
// the condition is gone, as on hardware, so the model keeps no latched bits.
//
using System.Collections.Generic;
using Antmicro.Renode.Core;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.UART
{
    public class LusaEsp32C3Uart : UARTBase, IDoubleWordPeripheral, IKnownSize
    {
        public LusaEsp32C3Uart(IMachine machine) : base(machine)
        {
            IRQ = new GPIO();
            Reset();
        }

        public override void Reset()
        {
            base.Reset();
            interruptEnable = 0;
            clockDivider = 0x2B6;
            conf0 = 0x1C;
            conf1 = 0x60 | (0x60 << 9);
            clockConf = 0x3701000;
            IRQ.Unset();
        }

        public uint ReadDoubleWord(long offset)
        {
            switch(offset)
            {
            case Fifo:
                if(!TryGetCharacter(out var character))
                {
                    this.Log(LogLevel.Warning, "Read from an empty receive FIFO");
                }
                Update();
                return character;
            case RawInterrupts:
                return Raw;
            case MaskedInterrupts:
                return Raw & interruptEnable;
            case InterruptEnable:
                return interruptEnable;
            case ClockDivider:
                return clockDivider;
            case StatusRegister:
                return (uint)System.Math.Min(Count, 0x3FF) | 0xE000C000;
            case Conf0:
                return conf0;
            case Conf1:
                return conf1;
            case ClockConf:
                return clockConf;
            }
            this.Log(LogLevel.Warning, "Unhandled read at 0x{0:X}", offset);
            return 0;
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            switch(offset)
            {
            case Fifo:
                TransmitCharacter((byte)value);
                break;
            case InterruptEnable:
                interruptEnable = value & 0x7FFFF;
                break;
            case InterruptClear:
                // Both modelled interrupts are level conditions.
                break;
            case ClockDivider:
                clockDivider = value;
                break;
            case Conf0:
                conf0 = value;
                break;
            case Conf1:
                conf1 = value;
                break;
            case ClockConf:
                clockConf = value;
                break;
            default:
                this.Log(LogLevel.Warning, "Unhandled write at 0x{0:X}: 0x{1:X}", offset, value);
                return;
            }
            Update();
        }

        public long Size => 0x100;

        public GPIO IRQ { get; }

        public override Bits StopBits => Bits.One;

        public override Parity ParityBit => Parity.None;

        public override uint BaudRate => 115200;

        protected override void CharWritten()
        {
            Update();
        }

        protected override void QueueEmptied()
        {
            Update();
        }

        private uint Raw
        {
            get
            {
                uint raw = 0;
                var receiveThreshold = conf1 & 0x1FF;
                var transmitThreshold = (conf1 >> 9) & 0x1FF;
                if(Count > 0 && Count >= receiveThreshold)
                {
                    raw |= 0x1;
                }
                if(transmitThreshold > 0)
                {
                    raw |= 0x2;                     // transmit FIFO is empty
                }
                return raw;
            }
        }

        private void Update()
        {
            IRQ.Set((Raw & interruptEnable) != 0);
        }

        private uint interruptEnable;
        private uint clockDivider;
        private uint conf0;
        private uint conf1;
        private uint clockConf;

        private const long Fifo = 0x00;
        private const long RawInterrupts = 0x04;
        private const long MaskedInterrupts = 0x08;
        private const long InterruptEnable = 0x0C;
        private const long InterruptClear = 0x10;
        private const long ClockDivider = 0x14;
        private const long StatusRegister = 0x1C;
        private const long Conf0 = 0x20;
        private const long Conf1 = 0x24;
        private const long ClockConf = 0x78;
    }
}
