use epub_reader::{parse_markdown, parse_txt, Block, Style};

#[test]
fn txt_splits_paragraphs_on_blank_lines() {
    let doc = parse_txt(b"first line\nsame paragraph\n\nsecond paragraph\n").unwrap();
    let blocks = doc.chapters()[0].blocks();
    assert_eq!(blocks.len(), 2);
    match &blocks[0] {
        Block::Paragraph(spans) => {
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].text(), "first line same paragraph");
            assert_eq!(spans[0].style(), Style::empty());
        }
        other => panic!("expected paragraph, got {other:?}"),
    }
    match &blocks[1] {
        Block::Paragraph(spans) => assert_eq!(spans[0].text(), "second paragraph"),
        other => panic!("expected paragraph, got {other:?}"),
    }
}

#[test]
fn txt_rejects_non_utf8() {
    assert!(parse_txt(&[0xff, 0xfe]).is_err());
}

#[test]
fn txt_rejects_whitespace_only() {
    assert!(parse_txt(b"\n   \n\n").is_err());
}

#[test]
fn markdown_parses_headings_and_rule() {
    let doc = parse_markdown(b"# Title\n\nbody text\n\n---\n\n## Sub\n").unwrap();
    let blocks = doc.chapters()[0].blocks();
    assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
    assert!(matches!(blocks[1], Block::Paragraph(_)));
    assert!(matches!(blocks[2], Block::Rule));
    assert!(matches!(blocks[3], Block::Heading { level: 2, .. }));
}

#[test]
fn markdown_parses_inline_styles() {
    let doc = parse_markdown(b"plain **bold** and *italic* and `code` end\n").unwrap();
    let Block::Paragraph(spans) = &doc.chapters()[0].blocks()[0] else {
        panic!("expected paragraph");
    };
    let styled: Vec<(&str, Style)> = spans.iter().map(|s| (s.text(), s.style())).collect();
    assert_eq!(
        styled,
        vec![
            ("plain ", Style::empty()),
            ("bold", Style::BOLD),
            (" and ", Style::empty()),
            ("italic", Style::ITALIC),
            (" and ", Style::empty()),
            ("code", Style::CODE),
            (" end", Style::empty()),
        ]
    );
}

#[test]
fn markdown_unclosed_code_is_literal() {
    let doc = parse_markdown(b"a `b c\n").unwrap();
    let Block::Paragraph(spans) = &doc.chapters()[0].blocks()[0] else {
        panic!("expected paragraph");
    };
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text(), "a `b c");
    assert_eq!(spans[0].style(), Style::empty());
}
