"""Behavior fixtures for evidence calculations; no host writes or test-runner recursion."""
import unittest
import analyse as a


class Calculations(unittest.TestCase):
    def test_positional_fields_keep_missing_and_malformed_distinct_from_zero(self):
        self.assertIsNone(a.mm_stat(None)['ram_bytes'])
        self.assertIsNone(a.mm_stat('')['original_bytes'])
        bad = a.mm_stat('100 malformed 30 0 90 4')
        self.assertEqual((bad['original_bytes'], bad['compressed_bytes'], bad['ram_bytes']), (100, None, 30))
        self.assertEqual(a.mm_stat('0 0 0')['ram_bytes'], 0)
        self.assertIsNone(a.mm_stat('100 20')['ram_bytes'])
        self.assertIsNone(a.mm_stat('1 -2 3')['compressed_bytes'])
        self.assertIsNone(a.scalar(str(a.U64 + 1)))
        self.assertIsNone(a.scalar('9' * 5000))

    def test_capacity_original_payload_and_allocated_ram_are_not_interchangeable(self):
        # Large logical size is not measured RAM. Payload ratio differs from
        # effective ratio when allocation overhead/fragmentation is present.
        fields = a.mm_stat('800 200 250 0 300 0')
        self.assertEqual(a.ratio(fields['original_bytes'], fields['compressed_bytes']), 4)
        self.assertEqual(a.ratio(fields['original_bytes'], fields['ram_bytes']), 3.2)
        self.assertIsNone(a.ratio(0, 0))
        self.assertIsNone(a.ratio(None, 100))
        self.assertIsNone(a.ratio(100, 0))
        self.assertEqual(a.ratio(0, 100), 0)

    def test_swap_units_priority_and_allocated_slots(self):
        parsed = a.swaps('Filename Type Size Used Priority\n/dev/zram0 partition 1024 512 5\n/disk/swap file 2048 10 -1\n')
        self.assertEqual(parsed[0]['capacity_bytes'], 1024 * 1024)
        self.assertEqual(parsed[1]['allocated_bytes'], 10 * 1024)
        self.assertEqual(parsed[1]['priority'], -1)
        self.assertIsNone(a.swaps('Filename Type Size Used Priority\n/a file 10 0 ' + '9' * 5000))
        for bad in [None, '', 'bad header', 'Filename Type Size Used Priority\n/a file 10 11 0', 'Filename Type Size Used Priority\n/a file bad 0 0']:
            self.assertIsNone(a.swaps(bad))

    def test_reordered_whitespace_and_unrelated_fields_are_harmless(self):
        self.assertEqual(a.mm_stat('800 200 250'), {**a.mm_stat(' 800\t200   250\n'),})
        x = a.counters('pswpin 10\npswpout 20\n')
        y = a.counters('unrelated 999\npswpout 20\npswpin 10\n')
        self.assertEqual([x[k] for k in ['pswpin','pswpout']], [y[k] for k in ['pswpin','pswpout']])
        self.assertEqual(a.rate(x['pswpin'], 30, 5), 4)
        self.assertIsNone(a.counters('pswpin malformed')['pswpin'])
        self.assertIsNone(a.counters('pswpin 10\npswpin 20')['pswpin'])

    def test_rates_are_directional_and_clock_reset_or_missing_is_unknown(self):
        for t in [None, 0, -1, float('nan'), float('inf')]:
            self.assertIsNone(a.rate(1, 2, t))
        self.assertIsNone(a.rate(10, 5, 1))
        self.assertIsNone(a.rate(None, 5, 1))
        self.assertIsNone(a.rate(1, 5, 1, False))
        self.assertEqual(a.rate(5, 5, 2), 0)
        self.assertEqual(a.rate(10, 30, 5), 4)

    def test_memory_and_psi_missing_or_bad_units_stay_unknown(self):
        self.assertEqual(a.memory('MemAvailable: 1024 kB')['MemAvailable'], 1024 ** 2)
        self.assertNotIn('MemAvailable', a.memory('MemAvailable: 1024 MB'))
        self.assertIsNone(a.memory('MemAvailable: bad kB')['MemAvailable'])
        self.assertIsNone(a.psi('full avg10=NaN total=2')['full']['avg10_percent'])
        self.assertIsNone(a.psi('full avg10=101 total=2')['full']['avg10_percent'])
        self.assertEqual(a.psi('full total=200 avg10=2.5')['full']['total_microseconds'], 200)
        self.assertIsNone(a.psi('full total=1 total=2')['full']['total_microseconds'])

    def test_interval_separates_swap_directions_page_size_and_fault_events(self):
        def sample(time, text, boot='12345678-1234-1234-1234-123456789abc'):
            reads = {p: dict(text=v, monotonic_ns=time, started_at='fixture', error=None) for p,v in {
                '/proc/vmstat': text, '/proc/sys/kernel/random/boot_id': boot,
                '/proc/pressure/memory': 'full avg10=1 total=0',
                '/proc/pressure/io': 'full avg10=1 total=0',
            }.items()}
            return dict(reads=reads)
        before = sample(0, 'pswpin 10\npswpout 100\npgmajfault 20')
        after = sample(2_000_000_000, 'pswpin 14\npswpout 120\npgmajfault 22')
        r = a.interval(before, after, 8192)
        self.assertEqual(r['pswpin_per_second'], 2)
        self.assertEqual(r['pswpout_per_second'], 10)
        self.assertEqual(r['pgmajfault_per_second'], 1)
        self.assertEqual(r['pswpin_mib_per_second'], 2 * 8192 / 1024 ** 2)
        self.assertNotIn('pgmajfault_mib_per_second', r)
        after['reads']['/proc/sys/kernel/random/boot_id']['text'] += '\n'
        self.assertEqual(a.interval(before, after, 8192)['pswpin_per_second'], 2)
        after['reads']['/proc/vmstat'].pop('monotonic_ns')
        self.assertIsNone(a.interval(before, after, 8192)['pswpin_per_second'])
        after = sample(2_000_000_000, 'pswpin 14', boot='different')
        self.assertIsNone(a.interval(before, after, 8192)['pswpin_per_second'])


if __name__ == '__main__':
    unittest.main()
