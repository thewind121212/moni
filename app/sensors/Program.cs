// moni-sensors: prints every LibreHardwareMonitor sensor as one JSON line per
// interval on stdout. moni-service starts it, reads the lines, and picks the
// CPU temperature/power/fan/clock and motherboard/SSD temperatures from them.
// When its parent closes stdin it closes LibreHardwareMonitor (releasing the
// PawnIO driver and drive handles) and exits, or is forced out after 1.5 s.
using System;
using System.Globalization;
using System.IO;
using System.Text;
using System.Threading;
using LibreHardwareMonitor.Hardware;

namespace Moni.Sensors
{
    internal static class Program
    {
        private static int Main(string[] args)
        {
            int interval = 1000;
            bool once = false;
            for (int i = 0; i < args.Length; i++)
            {
                if (args[i] == "--once") once = true;
                else if (args[i] == "--interval" && i + 1 < args.Length) int.TryParse(args[++i], out interval);
            }
            interval = Math.Max(250, interval);

            var stdout = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = true };
            var stop = new ManualResetEvent(false);
            if (!once)
            {
                // Parent gone (or service stopping) -> stdin hits EOF -> close and exit.
                var watcher = new Thread(() =>
                {
                    try { while (Console.In.Read() != -1) { } } catch { }
                    stop.Set();
                    Thread.Sleep(1500); // the main loop normally returns well before this
                    Environment.Exit(0);
                }) { IsBackground = true };
                watcher.Start();
            }

            Computer computer = null;
            while (true)
            {
                if (computer == null)
                {
                    try
                    {
                        computer = new Computer
                        {
                            IsCpuEnabled = true,
                            IsMotherboardEnabled = true,
                            IsStorageEnabled = true,
                            IsPsuEnabled = true,
                            IsControllerEnabled = true,
                            // NVIDIA comes straight from NVML in the service.
                            IsGpuEnabled = false,
                            IsMemoryEnabled = false,
                            IsNetworkEnabled = false,
                            IsBatteryEnabled = false,
                        };
                        computer.Open();
                    }
                    catch (Exception error)
                    {
                        stdout.WriteLine("{\"error\":" + Quote(error.Message) + "}");
                        try { computer?.Close(); } catch { }
                        computer = null;
                        if (once) return 1;
                        if (stop.WaitOne(5000)) return 0;
                        continue;
                    }
                }

                var line = new StringBuilder(16384);
                line.Append("{\"rows\":[");
                bool first = true;
                try
                {
                    foreach (IHardware hardware in computer.Hardware)
                        Visit(hardware, line, ref first);
                }
                catch (Exception error)
                {
                    stdout.WriteLine("{\"error\":" + Quote(error.Message) + "}");
                    if (stop.WaitOne(interval)) break;
                    continue;
                }
                line.Append("]}");
                stdout.WriteLine(line.ToString());
                if (once || stop.WaitOne(interval)) break;
            }
            try { computer?.Close(); } catch { }
            return 0;
        }

        private static void Visit(IHardware hardware, StringBuilder line, ref bool first)
        {
            hardware.Update();
            foreach (IHardware sub in hardware.SubHardware)
                Visit(sub, line, ref first);
            foreach (ISensor sensor in hardware.Sensors)
            {
                float value = sensor.Value ?? 0f;
                if (float.IsNaN(value) || float.IsInfinity(value)) value = 0f;
                if (!first) line.Append(',');
                first = false;
                line.Append("{\"name\":").Append(Quote(sensor.Name))
                    .Append(",\"type\":").Append(Quote(sensor.SensorType.ToString()))
                    .Append(",\"id\":").Append(Quote(sensor.Identifier.ToString()))
                    .Append(",\"parent\":").Append(Quote(hardware.Identifier.ToString()))
                    .Append(",\"hw\":").Append(Quote(hardware.Name))
                    .Append(",\"value\":").Append(value.ToString("R", CultureInfo.InvariantCulture))
                    .Append('}');
            }
        }

        private static string Quote(string text)
        {
            var quoted = new StringBuilder((text?.Length ?? 0) + 2);
            quoted.Append('"');
            foreach (char c in text ?? "")
            {
                switch (c)
                {
                    case '"': quoted.Append("\\\""); break;
                    case '\\': quoted.Append("\\\\"); break;
                    case '\n': quoted.Append("\\n"); break;
                    case '\r': quoted.Append("\\r"); break;
                    case '\t': quoted.Append("\\t"); break;
                    default:
                        if (c < 0x20) quoted.Append("\\u").Append(((int)c).ToString("x4"));
                        else quoted.Append(c);
                        break;
                }
            }
            return quoted.Append('"').ToString();
        }
    }
}
