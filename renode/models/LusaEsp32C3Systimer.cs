//
// Lusa repository model: ESP32-C3 system timer (SYSTIMER).
//
// Renode 1.17 has no ESP32-C3 SYSTIMER.  This one covers counter unit 0 and
// alarm 0, which `lusa::hal::timer` uses as the executor tick:
//
//   CONF +0x00 (CLK_EN bit 31, TIMER_UNIT0_WORK_EN bit 30,
//   TARGET0_WORK_EN bit 24), UNIT0_OP +0x04 (UPDATE bit 30, VALUE_VALID
//   bit 29), TARGET0_HI/LO +0x1C/+0x20, TARGET0_CONF +0x34 (PERIOD bits
//   0..25, PERIOD_MODE bit 30), UNIT0_VALUE_HI/LO +0x40/+0x44,
//   COMP0_LOAD +0x50, INT_ENA +0x64, INT_RAW +0x68, INT_CLR +0x6C,
//   INT_ST +0x70, DATE +0xFC.
//
// The counter runs at 16 MHz.  COMP0_LOAD arms alarm 0: in period mode it
// fires every PERIOD counts, otherwise once when the counter reaches TARGET0.
// The alarm counts while CONF enables both unit 0 and target 0.  Each alarm
// sets INT_RAW bit 0, and the IRQ line follows INT_RAW & INT_ENA.
//
using Antmicro.Renode.Core;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.Timers
{
    public class LusaEsp32C3Systimer : IDoubleWordPeripheral, IKnownSize
    {
        public LusaEsp32C3Systimer(IMachine machine)
        {
            IRQ = new GPIO();
            counter = new LimitTimer(machine.ClockSource, Frequency, this, "unit0", limit: ulong.MaxValue, direction: Direction.Ascending, enabled: true, workMode: WorkMode.Periodic);
            alarm = new LimitTimer(machine.ClockSource, Frequency, this, "alarm0", limit: 1, direction: Direction.Ascending, enabled: false, workMode: WorkMode.Periodic, eventEnabled: true);
            alarm.LimitReached += OnAlarm;
            Reset();
        }

        public void Reset()
        {
            config = ConfigReset;
            targetConfig = 0;
            targetHigh = 0;
            targetLow = 0;
            latched = 0;
            interruptEnable = 0;
            interruptRaw = 0;
            loaded = false;
            counter.Reset();
            counter.Enabled = true;
            alarm.Reset();
            alarm.Enabled = false;
            IRQ.Unset();
        }

        public uint ReadDoubleWord(long offset)
        {
            switch(offset)
            {
            case Config:
                return config;
            case Unit0Operation:
                return Unit0ValueValid;
            case Target0High:
                return targetHigh;
            case Target0Low:
                return targetLow;
            case Target0Config:
                return targetConfig;
            case Unit0ValueHigh:
                return (uint)(latched >> 32);
            case Unit0ValueLow:
                return (uint)latched;
            case InterruptEnable:
                return interruptEnable;
            case InterruptRaw:
                return interruptRaw;
            case InterruptStatus:
                return interruptRaw & interruptEnable;
            case Date:
                return DateValue;
            }
            this.Log(LogLevel.Warning, "Unhandled read at 0x{0:X}", offset);
            return 0;
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            switch(offset)
            {
            case Config:
                config = value;
                UpdateAlarm();
                break;
            case Unit0Operation:
                if((value & Unit0Update) != 0)
                {
                    latched = counter.Value;
                }
                break;
            case Target0High:
                targetHigh = value & 0xFFFFF;
                break;
            case Target0Low:
                targetLow = value;
                break;
            case Target0Config:
                targetConfig = value;
                break;
            case Comp0Load:
                if((value & 1) != 0)
                {
                    Arm();
                }
                break;
            case InterruptEnable:
                interruptEnable = value & 0x7;
                break;
            case InterruptClear:
                interruptRaw &= ~value;
                break;
            default:
                this.Log(LogLevel.Warning, "Unhandled write at 0x{0:X}: 0x{1:X}", offset, value);
                break;
            }
            RefreshIrq();
        }

        public long Size => 0x100;

        public GPIO IRQ { get; }

        private void Arm()
        {
            ulong limit;
            if((targetConfig & PeriodMode) != 0)
            {
                limit = targetConfig & PeriodMask;
            }
            else
            {
                var target = ((ulong)targetHigh << 32) | targetLow;
                var now = counter.Value;
                limit = target > now ? target - now : 1;
            }
            alarm.Enabled = false;
            alarm.Limit = limit == 0 ? 1 : limit;
            alarm.Value = 0;
            loaded = true;
            UpdateAlarm();
        }

        private void UpdateAlarm()
        {
            alarm.Enabled = loaded && (config & Target0WorkEnable) != 0 && (config & Unit0WorkEnable) != 0;
        }

        private void OnAlarm()
        {
            interruptRaw |= 1;
            if((targetConfig & PeriodMode) == 0)
            {
                loaded = false;
                alarm.Enabled = false;
            }
            RefreshIrq();
        }

        private void RefreshIrq()
        {
            IRQ.Set((interruptRaw & interruptEnable) != 0);
        }

        private readonly LimitTimer counter;
        private readonly LimitTimer alarm;
        private uint config;
        private uint targetConfig;
        private uint targetHigh;
        private uint targetLow;
        private ulong latched;
        private uint interruptEnable;
        private uint interruptRaw;
        private bool loaded;

        private const long Frequency = 16000000;
        private const long Config = 0x00;
        private const long Unit0Operation = 0x04;
        private const long Target0High = 0x1C;
        private const long Target0Low = 0x20;
        private const long Target0Config = 0x34;
        private const long Unit0ValueHigh = 0x40;
        private const long Unit0ValueLow = 0x44;
        private const long Comp0Load = 0x50;
        private const long InterruptEnable = 0x64;
        private const long InterruptRaw = 0x68;
        private const long InterruptClear = 0x6C;
        private const long InterruptStatus = 0x70;
        private const long Date = 0xFC;
        private const uint ConfigReset = 0x46000000;
        private const uint DateValue = 0x2006171;
        private const uint Unit0Update = 1u << 30;
        private const uint Unit0ValueValid = 1u << 29;
        private const uint Unit0WorkEnable = 1u << 30;
        private const uint Target0WorkEnable = 1u << 24;
        private const uint PeriodMode = 1u << 30;
        private const uint PeriodMask = 0x3FFFFFF;
    }
}
