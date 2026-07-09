import unittest
from whisper_stream import (
    parse_hotwords,
    parse_corrections,
    apply_corrections,
    load_hotwords,
    load_corrections,
    is_hallucination,
)


class TestHallucinations(unittest.TestCase):
    def test_prodolzhenie_sleduet_dropped(self):
        self.assertTrue(is_hallucination("Продолжение следует..."))
        self.assertTrue(is_hallucination("продолжение следует"))

    def test_subtitle_credits_dropped(self):
        self.assertTrue(is_hallucination("Субтитры сделал DimaTorzok"))
        self.assertTrue(
            is_hallucination("Редактор субтитров А.Семкин Корректор А.Егорова")
        )

    def test_empty_dropped(self):
        self.assertTrue(is_hallucination("   ...  "))

    def test_high_no_speech_prob_dropped(self):
        self.assertTrue(is_hallucination("любой текст", no_speech_prob=0.9))

    def test_sound_event_tags_dropped(self):
        # Частые звуковые теги из субтитров, которые whisper вставляет на тонком аудио.
        for t in ("Аплодисменты", "аплодисменты.", "АПЛОДИСМЕНТЫ", "Музыка", "Смех"):
            self.assertTrue(is_hallucination(t), t)

    def test_bracketed_tags_dropped(self):
        # Любой сегмент целиком в скобках — не-речь, независимо от содержимого.
        for t in ("[Аплодисменты]", "(музыка)", "[смех]", "[ Музыка ]", "(звонок телефона)"):
            self.assertTrue(is_hallucination(t), t)

    def test_youtube_outro_dropped(self):
        self.assertTrue(is_hallucination("Спасибо за просмотр!"))
        self.assertTrue(is_hallucination("Подписывайтесь на канал"))

    def test_digits_kept(self):
        # Главный кейс пользователя: диктует числа — их нельзя отсеивать.
        self.assertFalse(is_hallucination("один два три четыре"))
        self.assertFalse(is_hallucination("1, 2, 3, 4"))
        self.assertFalse(is_hallucination("пять"))

    def test_real_speech_kept(self):
        self.assertFalse(is_hallucination("задеплоил на стейджинг"))
        self.assertFalse(is_hallucination("подниму кластер", no_speech_prob=0.2))
        # «Спасибо» само по себе — обычная речь, не глушим.
        self.assertFalse(is_hallucination("спасибо"))

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

    def test_replacement_with_backslash_is_literal(self):
        # users hand-edit the TSV; a backslash in the replacement must stay literal
        pairs = parse_corrections(["сиошарп\tC\\#"])
        self.assertEqual(apply_corrections("это сиошарп", pairs), "это C\\#")

    def test_replacement_group_ref_not_interpreted(self):
        # "\1" must not raise re.error nor be treated as a backreference
        pairs = parse_corrections(["ромб\t\\1"])
        self.assertEqual(apply_corrections("ромб тут", pairs), "\\1 тут")

class TestLoaders(unittest.TestCase):
    def test_load_hotwords_missing_file(self):
        self.assertEqual(load_hotwords("/nonexistent/path/it_hotwords.txt"), "")

    def test_load_corrections_missing_file(self):
        self.assertEqual(load_corrections("/nonexistent/path/it_corrections.tsv"), [])

if __name__ == "__main__":
    unittest.main()
