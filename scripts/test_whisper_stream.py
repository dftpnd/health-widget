import unittest
from whisper_stream import parse_hotwords, parse_corrections, apply_corrections

class TestHotwords(unittest.TestCase):
    def test_joins_terms(self):
        self.assertEqual(parse_hotwords(["Kubernetes", "Docker"]), "Kubernetes, Docker")

    def test_skips_blank_and_comments(self):
        self.assertEqual(parse_hotwords(["Kubernetes", "", "# note", "Docker"]),
                         "Kubernetes, Docker")

    def test_respects_limit(self):
        self.assertEqual(parse_hotwords(["a", "b", "c"], limit=2), "a, b")

class TestCorrections(unittest.TestCase):
    def test_basic_replace(self):
        pairs = parse_corrections(["кубернетис\tKubernetes"])
        self.assertEqual(apply_corrections("запусти кубернетис сегодня", pairs),
                         "запусти Kubernetes сегодня")

    def test_case_insensitive(self):
        pairs = parse_corrections(["дэплой\tdeploy"])
        self.assertEqual(apply_corrections("Дэплой прошёл", pairs), "deploy прошёл")

    def test_word_boundary(self):
        # не трогаем часть более длинного слова
        pairs = parse_corrections(["код\tcode"])
        self.assertEqual(apply_corrections("кодовое слово", pairs), "кодовое слово")

    def test_multiword_phrase(self):
        pairs = parse_corrections(["пул реквест\tpull request"])
        self.assertEqual(apply_corrections("сделай пул реквест", pairs),
                         "сделай pull request")

    def test_skips_malformed_lines(self):
        pairs = parse_corrections(["нет таба", "", "# коммент", "a\tb"])
        self.assertEqual(len(pairs), 1)

if __name__ == "__main__":
    unittest.main()
