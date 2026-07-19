import unittest
from whisper_stream import (
    parse_hotwords,
    parse_corrections,
    apply_corrections,
    load_hotwords,
    load_corrections,
    is_hallucination,
    texts_agree,
    norm_word,
    common_prefix,
    advance,
    take_final,
)

class TestTextsAgree(unittest.TestCase):
    def test_identical_agree(self):
        self.assertTrue(texts_agree("Да, согласен.", "Да, согласен."))

    def test_case_and_punct_ignored(self):
        self.assertTrue(texts_agree("Нет.", "нет"))
        self.assertTrue(texts_agree("хорошо, спасибо, до свидания",
                                    "Хорошо! Спасибо. До свидания"))

    def test_minor_ending_variation_agrees(self):
        self.assertTrue(texts_agree("Отпочковалась на несколько.",
                                    "Отпочковалось на несколько."))
        self.assertTrue(texts_agree("только окладное", "только окладная"))

    def test_containment_agrees(self):
        self.assertTrue(texts_agree("Это стрессовая штука.", "стрессовая"))
        self.assertTrue(texts_agree("нам нравилось", "И все, что нам нравилось."))

    def test_unstable_decodes_disagree(self):
        self.assertFalse(texts_agree("и т.д.", "Продолжение следует..."))
        self.assertFalse(texts_agree("Здравствуйте!", "Тарас."))
        self.assertFalse(texts_agree("ПОДПИШИСЬ НА КАНАЛ!",
                                     "ПРОДОЛЖЕНИЕ В СЛЕДУЮЩЕЙ ЧАСТИ"))
        self.assertFalse(texts_agree("1, 2, 3.", "раз, два, три"))

    def test_one_side_empty_disagrees(self):
        self.assertFalse(texts_agree("и т.д.", ""))
        self.assertFalse(texts_agree("", "Продолжение следует..."))

    def test_both_empty_agree(self):
        self.assertTrue(texts_agree("", ""))

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

    def test_high_no_speech_prob_with_poor_decode_dropped(self):
        self.assertTrue(is_hallucination("любой текст", no_speech_prob=0.9,
                                         avg_logprob=-0.8))

    def test_high_no_speech_prob_with_good_decode_kept(self):
        self.assertFalse(is_hallucination(
            "Это универсальная штука для геймеров, для пользователей.",
            no_speech_prob=0.985, avg_logprob=-0.28))

    def test_sound_event_tags_dropped(self):
        for t in ("Аплодисменты", "аплодисменты.", "АПЛОДИСМЕНТЫ", "Музыка", "Смех"):
            self.assertTrue(is_hallucination(t), t)

    def test_bracketed_tags_dropped(self):
        for t in ("[Аплодисменты]", "(музыка)", "[смех]", "[ Музыка ]", "(звонок телефона)"):
            self.assertTrue(is_hallucination(t), t)

    def test_youtube_outro_dropped(self):
        self.assertTrue(is_hallucination("Спасибо за просмотр!"))
        self.assertTrue(is_hallucination("Подписывайтесь на канал"))

    def test_digits_kept(self):
        self.assertFalse(is_hallucination("один два три четыре"))
        self.assertFalse(is_hallucination("1, 2, 3, 4"))
        self.assertFalse(is_hallucination("пять"))

    def test_real_speech_kept(self):
        self.assertFalse(is_hallucination("задеплоил на стейджинг"))
        self.assertFalse(is_hallucination("подниму кластер", no_speech_prob=0.2))
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
        pairs = parse_corrections(["сиошарп\tC\\#"])
        self.assertEqual(apply_corrections("это сиошарп", pairs), "это C\\#")

    def test_replacement_group_ref_not_interpreted(self):
        pairs = parse_corrections(["ромб\t\\1"])
        self.assertEqual(apply_corrections("ромб тут", pairs), "\\1 тут")

class TestLoaders(unittest.TestCase):
    def test_load_hotwords_missing_file(self):
        self.assertEqual(load_hotwords("/nonexistent/path/it_hotwords.txt"), "")

    def test_load_corrections_missing_file(self):
        self.assertEqual(load_corrections("/nonexistent/path/it_corrections.tsv"), [])

class TestNormWord(unittest.TestCase):
    def test_lower_and_strip_punct(self):
        self.assertEqual(norm_word("Привет,"), "привет")
        self.assertEqual(norm_word("мир."), "мир")

    def test_only_punct_is_empty(self):
        self.assertEqual(norm_word("—"), "")

class TestCommonPrefix(unittest.TestCase):
    def test_full_match_ignores_case_and_punct(self):
        a = [("Привет,", 0.0, 0.4), ("мир", 0.5, 0.9)]
        b = [("привет", 0.0, 0.4), ("мир.", 0.5, 0.9)]
        self.assertEqual(common_prefix(a, b), 2)

    def test_divergence_stops_prefix(self):
        a = [("раз", 0.0, 0.3), ("два", 0.4, 0.7), ("три", 0.8, 1.1)]
        b = [("раз", 0.0, 0.3), ("двадцать", 0.4, 0.7), ("три", 0.8, 1.1)]
        self.assertEqual(common_prefix(a, b), 1)

    def test_empty_side(self):
        self.assertEqual(common_prefix([], [("а", 0.0, 0.3)]), 0)

class TestAdvance(unittest.TestCase):
    def test_agreed_prefix_commits(self):
        prev = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0)]
        cur = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0), ("завтра", 1.1, 1.5)]
        committed, newly, partial = advance(prev, 0, cur)
        self.assertEqual(committed, 2)
        self.assertEqual(newly, ["запусти", "кластер"])
        self.assertEqual(partial, "завтра")

    def test_already_committed_not_repeated(self):
        prev = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0), ("завтра", 1.1, 1.5)]
        cur = [("запусти", 0.0, 0.5), ("кластер", 0.6, 1.0), ("завтра", 1.1, 1.5)]
        committed, newly, partial = advance(prev, 2, cur)
        self.assertEqual(committed, 3)
        self.assertEqual(newly, ["завтра"])
        self.assertEqual(partial, "")

    def test_shifted_hypothesis_commits_nothing(self):
        prev = [("шум", 0.0, 0.5)]
        cur = [("совсем", 0.0, 0.4), ("другое", 0.5, 0.9)]
        committed, newly, partial = advance(prev, 0, cur)
        self.assertEqual(committed, 0)
        self.assertEqual(newly, [])
        self.assertEqual(partial, "совсем другое")

    def test_rewrite_before_committed_ignored(self):
        prev = [("раз", 0.0, 0.3), ("два", 0.4, 0.7)]
        cur = [("уже", 0.0, 0.3), ("другой", 0.4, 0.7), ("текст", 0.8, 1.1)]
        committed, newly, partial = advance(prev, 2, cur)
        self.assertEqual(committed, 2)
        self.assertEqual(newly, [])
        self.assertEqual(partial, "текст")

class TestTakeFinal(unittest.TestCase):
    def test_no_boundary_keeps_pending(self):
        out, rest = take_final(["привет", "мир"])
        self.assertEqual(out, "")
        self.assertEqual(rest, ["привет", "мир"])

    def test_cuts_at_last_sentence_end(self):
        out, rest = take_final(["Привет.", "Как", "дела?", "Я"])
        self.assertEqual(out, "Привет. Как дела?")
        self.assertEqual(rest, ["Я"])

    def test_ellipsis_is_boundary(self):
        out, rest = take_final(["ну…", "и"])
        self.assertEqual(out, "ну…")
        self.assertEqual(rest, ["и"])

    def test_word_limit_flushes_all(self):
        words = ["слово"] * 30
        out, rest = take_final(words)
        self.assertEqual(out, " ".join(words))
        self.assertEqual(rest, [])

    def test_empty_pending(self):
        out, rest = take_final([])
        self.assertEqual(out, "")
        self.assertEqual(rest, [])

if __name__ == "__main__":
    unittest.main()
