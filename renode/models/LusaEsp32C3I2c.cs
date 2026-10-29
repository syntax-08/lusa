//
// Lusa repository model: ESP32-C3 I2C controller (master, command list).
//
// Renode 1.17 has no ESP32-C3 I2C model.  This one runs the controller's
// command list when CTR.TRANS_START (bit 5) is written:
//
//   CTR +0x04, SR +0x08 (RXFIFO_CNT 13:8, TXFIFO_CNT 23:18),
//   FIFO_CONF +0x18 (RX_FIFO_RST bit 12, TX_FIFO_RST bit 13), DATA +0x1C,
//   INT_RAW +0x20, INT_CLR +0x24, INT_ENA +0x28, INT_STATUS +0x2C,
//   COMD0..7 +0x58..+0x74 (BYTE_NUM 7:0, ACK_CHECK_EN 8, ACK_VALUE 10,
//   OP_CODE 13:11 -- RSTART 6, WRITE 1, READ 3, STOP 2, END 4;
//   COMMAND_DONE bit 31).
//
// The first byte written after RSTART addresses a device registered on this
// controller (`@ i2c0 0x55`).  An unknown address with ACK checking raises
// NACK (bit 10) and stops the list; STOP delivers the written bytes, ends the
// transaction, and raises TRANS_COMPLETE (bit 7); END raises END_DETECT
// (bit 3).  Every other register reads back what was written.
//
using System.Collections.Generic;
using System.Linq;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.I2C
{
    public class LusaEsp32C3I2c : SimpleContainer<II2CPeripheral>, IDoubleWordPeripheral, IKnownSize
    {
        public LusaEsp32C3I2c(IMachine machine) : base(machine)
        {
            IRQ = new GPIO();
            Reset();
        }

        public override void Reset()
        {
            System.Array.Clear(registers, 0, registers.Length);
            transmitFifo.Clear();
            receiveFifo.Clear();
            interruptRaw = 0;
            interruptEnable = 0;
            IRQ.Unset();
        }

        public uint ReadDoubleWord(long offset)
        {
            switch(offset)
            {
            case Status:
                return ((uint)receiveFifo.Count << 8) | ((uint)transmitFifo.Count << 18);
            case Data:
                var value = receiveFifo.Count > 0 ? receiveFifo.Dequeue() : (byte)0;
                return value;
            case InterruptRaw:
                return interruptRaw;
            case InterruptEnable:
                return interruptEnable;
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
            switch(offset)
            {
            case Control:
                registers[Control / 4] = value & ~(TransStart | ConfUpgate);
                if((value & TransStart) != 0)
                {
                    RunCommands();
                }
                break;
            case FifoConf:
                registers[FifoConf / 4] = value;
                if((value & (1u << 12)) != 0)
                {
                    receiveFifo.Clear();
                }
                if((value & (1u << 13)) != 0)
                {
                    transmitFifo.Clear();
                }
                break;
            case Data:
                if(transmitFifo.Count < FifoDepth)
                {
                    transmitFifo.Enqueue((byte)value);
                }
                else
                {
                    interruptRaw |= TxFifoOverflow;
                }
                break;
            case InterruptClear:
                interruptRaw &= ~value;
                break;
            case InterruptEnable:
                interruptEnable = value;
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

        private void RunCommands()
        {
            II2CPeripheral device = null;
            var addressPending = false;
            var written = new List<byte>();
            for(var index = 0; index < 8; index++)
            {
                var slot = (CommandBase / 4) + index;
                var command = registers[slot];
                var opcode = (command >> 11) & 0x7;
                var count = (int)(command & 0xFF);
                var checkAck = (command & (1u << 8)) != 0;
                registers[slot] = command | CommandDone;
                switch(opcode)
                {
                case OpRestart:
                    Deliver(device, written);
                    device = null;
                    addressPending = true;
                    break;
                case OpWrite:
                    for(var i = 0; i < count; i++)
                    {
                        if(transmitFifo.Count == 0)
                        {
                            break;
                        }
                        var data = transmitFifo.Dequeue();
                        if(addressPending)
                        {
                            addressPending = false;
                            if(!TryGetByAddress(data >> 1, out device))
                            {
                                device = null;
                                if(checkAck)
                                {
                                    this.Log(LogLevel.Debug, "No device at 0x{0:X}: NACK", data >> 1);
                                    interruptRaw |= Nack;
                                    return;
                                }
                            }
                            continue;
                        }
                        written.Add(data);
                    }
                    break;
                case OpRead:
                    Deliver(device, written);
                    var replies = device?.Read(count) ?? new byte[0];
                    foreach(var reply in replies.Concat(Enumerable.Repeat((byte)0, count)).Take(count))
                    {
                        if(receiveFifo.Count < FifoDepth)
                        {
                            receiveFifo.Enqueue(reply);
                        }
                    }
                    break;
                case OpStop:
                    Deliver(device, written);
                    device?.FinishTransmission();
                    interruptRaw |= TransComplete;
                    return;
                case OpEnd:
                    Deliver(device, written);
                    interruptRaw |= EndDetect;
                    return;
                default:
                    this.Log(LogLevel.Warning, "Unsupported I2C opcode {0} in COMD{1}", opcode, index);
                    return;
                }
            }
        }

        private void Deliver(II2CPeripheral device, List<byte> written)
        {
            if(device != null && written.Count > 0)
            {
                device.Write(written.ToArray());
            }
            written.Clear();
        }

        private void RefreshIrq()
        {
            IRQ.Set((interruptRaw & interruptEnable) != 0);
        }

        private uint interruptRaw;
        private uint interruptEnable;
        private readonly uint[] registers = new uint[0x100 / 4];
        private readonly Queue<byte> transmitFifo = new Queue<byte>();
        private readonly Queue<byte> receiveFifo = new Queue<byte>();

        private const int FifoDepth = 32;
        private const long Control = 0x04;
        private const long Status = 0x08;
        private const long FifoConf = 0x18;
        private const long Data = 0x1C;
        private const long InterruptRaw = 0x20;
        private const long InterruptClear = 0x24;
        private const long InterruptEnable = 0x28;
        private const long InterruptStatus = 0x2C;
        private const long CommandBase = 0x58;
        private const uint TransStart = 1u << 5;
        private const uint ConfUpgate = 1u << 11;
        private const uint CommandDone = 1u << 31;
        private const uint EndDetect = 1u << 3;
        private const uint TransComplete = 1u << 7;
        private const uint Nack = 1u << 10;
        private const uint TxFifoOverflow = 1u << 11;
        private const uint OpRestart = 6;
        private const uint OpWrite = 1;
        private const uint OpRead = 3;
        private const uint OpStop = 2;
        private const uint OpEnd = 4;
    }
}
