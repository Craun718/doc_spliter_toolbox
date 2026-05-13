fn main() {
    let mut doc = lopdf::Document::new();
    let font_id = doc.add_object(lopdf::Dictionary::from_iter(vec![
        ("Type", lopdf::Object::Name(b"Font".to_vec())),
        ("Subtype", lopdf::Object::Name(b"Type1".to_vec())),
        ("BaseFont", lopdf::Object::Name(b"Times-Roman".to_vec())),
    ]));

    let mut page_ids = Vec::new();
    for i in 0..30 {
        let content = format!("BT /F1 12 Tf 100 700 Td (Page {}) Tj ET", i + 1);
        let content_id = doc.add_object(lopdf::Stream::new(
            lopdf::Dictionary::from_iter(vec![
                ("Length", lopdf::Object::Integer(content.len() as i64)),
            ]),
            content.into_bytes(),
        ));
        let page_id = doc.new_object_id();
        let page_dict = lopdf::Dictionary::from_iter(vec![
            ("Type", lopdf::Object::Name(b"Page".to_vec())),
            ("MediaBox", lopdf::Object::Array(vec![
                lopdf::Object::Integer(0), lopdf::Object::Integer(0),
                lopdf::Object::Integer(612), lopdf::Object::Integer(792),
            ])),
            ("Contents", lopdf::Object::Reference(content_id)),
            ("Resources", lopdf::Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                ("Font", lopdf::Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                    ("F1", lopdf::Object::Reference(font_id)),
                ]))),
            ]))),
        ]);
        doc.objects.insert(page_id, lopdf::Object::Dictionary(page_dict));
        page_ids.push(page_id);
    }

    let pages_id = doc.add_object(lopdf::Dictionary::from_iter(vec![
        ("Type", lopdf::Object::Name(b"Pages".to_vec())),
        ("Kids", lopdf::Object::Array(page_ids.iter().map(|&id| lopdf::Object::Reference(id)).collect())),
        ("Count", lopdf::Object::Integer(page_ids.len() as i64)),
    ]));

    for &page_id in &page_ids {
        if let Some(lopdf::Object::Dictionary(ref mut dict)) = doc.objects.get_mut(&page_id) {
            dict.set("Parent", lopdf::Object::Reference(pages_id));
        }
    }

    let catalog_id = doc.add_object(lopdf::Dictionary::from_iter(vec![
        ("Type", lopdf::Object::Name(b"Catalog".to_vec())),
        ("Pages", lopdf::Object::Reference(pages_id)),
    ]));
    doc.trailer.set("Root", lopdf::Object::Reference(catalog_id));

    doc.save("test_30pages.pdf").unwrap();
    println!("Created test_30pages.pdf with {} pages", page_ids.len());
}
