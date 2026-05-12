from sqlrite_agent.facts import extract_facts


def test_extracts_dog_name():
    facts = extract_facts("My dog's name is Mochi.")
    assert any(
        f.subject == "user.dog" and f.predicate == "name" and f.object == "Mochi"
        for f in facts
    )


def test_extracts_dog_name_alt_phrasing():
    facts = extract_facts("My dog is called Mochi.")
    assert any(
        f.subject == "user.dog" and f.predicate == "name" and f.object == "Mochi"
        for f in facts
    )


def test_extracts_location():
    facts = extract_facts("I'm from Lisbon.")
    assert any(
        f.subject == "user" and f.predicate == "location" and f.object == "Lisbon"
        for f in facts
    )


def test_extracts_favorite_thing():
    facts = extract_facts("My favorite color is blue.")
    assert any(
        f.subject == "user"
        and f.predicate == "favorite_color"
        and f.object == "blue"
        for f in facts
    )


def test_extracts_pet():
    facts = extract_facts("I have a cat named Whiskers.")
    assert any(
        f.subject == "user.cat"
        and f.predicate == "name"
        and f.object == "Whiskers"
        for f in facts
    )


def test_no_false_positives_on_neutral_text():
    facts = extract_facts("Hello there. The weather is fine.")
    assert facts == []


def test_dedupes_within_one_message():
    facts = extract_facts(
        "My dog's name is Mochi. By the way, my dog is called Mochi."
    )
    assert sum(1 for f in facts if f.object == "Mochi") == 1
