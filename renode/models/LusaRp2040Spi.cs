//
// Lusa repository model: ARM PrimeCell SSP (PL022) master, as used for the
// RP2040 SPI0/SPI1 blocks.  Renode 1.17 ships no PL022, so the Lusa test
// platforms load this file with `include @LusaRp2040Spi.cs`.
//
// Scope: master mode, 8-bit Motorola frames, an 8-entry receive FIFO, and the
// SSPIMSC/SSPRIS/SSPMIS/SSPICR interrupt registers.  A data-register write
// clocks one frame at once through the attached ISPIPeripheral (or the
// internal loopback when CR1.LBM is set) and stores the reply.  Each received
// frame raises the receive-timeout interrupt (RTRIS), which on hardware fires
// once the bus has been idle for 32 bit periods; RXRIS follows the
// half-full rule.  The transmit FIFO is always empty, so TXRIS stays set.
//
using System.Collections.Generic;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.SPI
{
    public class LusaRp2040Spi : NullRegistrationPointPeripheralContainer<ISPIPeripheral>, IDoubleWordPeripheral, IKnownSize
    {
        public LusaRp2040Spi(IMachine machine) : base(machine)
        {
            IRQ = new GPIO();
            Reset();
        }

        public override void Reset()
        {
            cr0 = 0;
            cr1 = 0;
            cpsr = 0;
            imsc = 0;
            dmacr = 0;
            overrun = false;
            timeout = false;
            receiveFifo.Clear();
            IRQ.Unset();
        }

        public uint ReadDoubleWord(long offset)
        {
            switch(offset)
            {
            case CR0: return cr0;
            case CR1: return cr1;
            case DR: return ReadData();
            case SR: return Status;
            case CPSR: return cpsr;
            case IMSC: return imsc;
            case RIS: return RawInterrupts;
            case MIS: return RawInterrupts & imsc;
            case DMACR: return dmacr;
            }
            if(offset >= 0xFE0 && offset <= 0xFFC)
            {
                return Identification[(offset - 0xFE0) / 4];
            }
            this.Log(LogLevel.Warning, "Unhandled read at 0x{0:X}", offset);
            return 0;
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            switch(offset)
            {
            case CR0:
                cr0 = value & 0xFFFF;
                if((cr0 & 0xF) != 7)
                {
                    this.Log(LogLevel.Warning, "Only 8-bit frames are modelled (DSS = {0})", cr0 & 0xF);
                }
                break;
            case CR1:
                var wasEnabled = Enabled;
                cr1 = value & 0xF;
                if(wasEnabled && !Enabled)
                {
                    RegisteredPeripheral?.FinishTransmission();
                }
                break;
            case DR:
                WriteData((byte)value);
                break;
            case CPSR:
                cpsr = value & 0xFE;
                break;
            case IMSC:
                imsc = value & 0xF;
                break;
            case ICR:
                if((value & 1) != 0)
                {
                    overrun = false;
                }
                if((value & 2) != 0)
                {
                    timeout = false;
                }
                break;
            case DMACR:
                dmacr = value & 0x3;
                break;
            default:
                this.Log(LogLevel.Warning, "Unhandled write at 0x{0:X}: 0x{1:X}", offset, value);
                return;
            }
            Update();
        }

        public long Size => 0x1000;

        public GPIO IRQ { get; }

        private bool Enabled => (cr1 & 0x2) != 0;

        private uint Status
        {
            get
            {
                uint status = 0x3;                          // TFE | TNF
                if(receiveFifo.Count > 0)
                {
                    status |= 0x4;                          // RNE
                }
                if(receiveFifo.Count >= FifoDepth)
                {
                    status |= 0x8;                          // RFF
                }
                return status;
            }
        }

        private uint RawInterrupts
        {
            get
            {
                uint raw = 0x8;                             // TXRIS: FIFO empty
                if(overrun)
                {
                    raw |= 0x1;
                }
                if(timeout && receiveFifo.Count > 0)
                {
                    raw |= 0x2;
                }
                if(receiveFifo.Count >= FifoDepth / 2)
                {
                    raw |= 0x4;
                }
                return raw;
            }
        }

        private void WriteData(byte value)
        {
            if(!Enabled)
            {
                this.Log(LogLevel.Warning, "Data written while SSE is clear; dropped 0x{0:X}", value);
                return;
            }
            byte reply;
            if((cr1 & 0x1) != 0)
            {
                reply = value;                              // LBM
            }
            else if(RegisteredPeripheral != null)
            {
                reply = RegisteredPeripheral.Transmit(value);
            }
            else
            {
                reply = 0;
            }
            if(receiveFifo.Count >= FifoDepth)
            {
                overrun = true;
            }
            else
            {
                receiveFifo.Enqueue(reply);
            }
            timeout = true;
        }

        private uint ReadData()
        {
            uint value = 0;
            if(receiveFifo.Count > 0)
            {
                value = receiveFifo.Dequeue();
            }
            Update();
            return value;
        }

        private void Update()
        {
            IRQ.Set((RawInterrupts & imsc) != 0);
        }

        private uint cr0;
        private uint cr1;
        private uint cpsr;
        private uint imsc;
        private uint dmacr;
        private bool overrun;
        private bool timeout;
        private readonly Queue<byte> receiveFifo = new Queue<byte>();

        private const int FifoDepth = 8;
        private const long CR0 = 0x00;
        private const long CR1 = 0x04;
        private const long DR = 0x08;
        private const long SR = 0x0C;
        private const long CPSR = 0x10;
        private const long IMSC = 0x14;
        private const long RIS = 0x18;
        private const long MIS = 0x1C;
        private const long ICR = 0x20;
        private const long DMACR = 0x24;
        private static readonly uint[] Identification = { 0x22, 0x10, 0x34, 0x00, 0x0D, 0xF0, 0x05, 0xB1 };
    }
}
