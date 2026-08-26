---
title: "Edge Case Testi ığüşçö"
alias: tekil-alias
tags: unicode-test
date: "2024-03-20"
custom_field: 42
---

# Kenar Durumları ığüşçö 😀

Bu dosya tüm edge case'leri test eder.

## Kod İçinde Sahte Linkler

```python
# Bu bir link DEĞİL: [[fake-link]]
x = "[[also-not-a-link]]"
# Bu da tag değil: #not-a-tag
```

Ama bu gerçek: [[gerçek-link]]

## Inline Code

Bu `[[inline-code-link]]` de link değil. Ve `#inline-tag` de tag değil.

Ama bu #gerçek-tag bir tag.

## Türkçe ve Emoji

İğneyle kuyu kazıyorum 😀🎉 ve [[türkçe-not#bölüm-başlığı]] referansı var.

## Boş ve Garip Linkler

[[]]
[[#]]
[[|sadece-display]]
[[normal-link]]
![[embed-dosya]]
[[dosya#^block-ref-123]]

## Footnote Test

Bu bir referans[^dipnot1] ve bu da[^dipnot2].

[^dipnot1]: İlk dipnot tanımı.
[^dipnot2]: İkinci dipnot tanımı.
