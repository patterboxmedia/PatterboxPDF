use super::{CommandError, Document, GroupId, HashMap, ObjectId, PhysicalSize, page_index};
use crate::model::{MasterId, MasterPage, ModelError};

pub(super) fn master_index(document: &Document, id: &MasterId) -> Result<usize, CommandError> {
    document
        .masters
        .iter()
        .position(|master| &master.id == id)
        .ok_or_else(|| ModelError::MasterNotFound(id.clone()).into())
}

pub(super) fn add_master(
    document: &mut Document,
    name: String,
    trim: PhysicalSize,
    created: &mut Vec<MasterId>,
) -> Result<(), CommandError> {
    if document.masters.len() >= Document::MAX_MASTERS {
        return Err(ModelError::TooManyMasters.into());
    }
    let id = document.allocate_master_id();
    document.masters.push(MasterPage {
        id: id.clone(),
        name,
        trim,
        objects: Vec::new(),
    });
    created.push(id);
    Ok(())
}

pub(super) fn rename_master(
    document: &mut Document,
    id: &MasterId,
    name: String,
) -> Result<(), CommandError> {
    let index = master_index(document, id)?;
    document.masters[index].name = name;
    Ok(())
}

pub(super) fn duplicate_master(
    document: &mut Document,
    id: &MasterId,
    name: String,
    created: &mut Vec<MasterId>,
    created_objects: &mut Vec<ObjectId>,
) -> Result<(), CommandError> {
    let index = master_index(document, id)?;
    let mut master = document.masters[index].clone();
    let mut groups = HashMap::<GroupId, GroupId>::new();
    for object in &mut master.objects {
        object.id = document.allocate_object_id();
        created_objects.push(object.id.clone());
        if let Some(group) = object.group_id.clone() {
            let mapped = if let Some(mapped) = groups.get(&group) {
                mapped.clone()
            } else {
                let mapped = document.allocate_group_id();
                groups.insert(group, mapped.clone());
                mapped
            };
            object.group_id = Some(mapped);
        }
    }
    add_master(document, name, master.trim, created)?;
    document
        .masters
        .last_mut()
        .expect("the added master exists")
        .objects = master.objects;
    Ok(())
}

pub(super) fn resize_master(
    document: &mut Document,
    id: &MasterId,
    trim: PhysicalSize,
) -> Result<(), CommandError> {
    let index = master_index(document, id)?;
    if document
        .pages
        .iter()
        .any(|page| page.master_id.as_ref() == Some(id) && page.trim != trim)
    {
        return Err(ModelError::MasterPageSizeMismatch.into());
    }
    document.masters[index].trim = trim;
    Ok(())
}

pub(super) fn assign_master(
    document: &mut Document,
    page_ids: &[crate::model::PageId],
    master_id: Option<&MasterId>,
) -> Result<(), CommandError> {
    let trim = master_id
        .map(|id| master_index(document, id).map(|index| document.masters[index].trim))
        .transpose()?;
    for index in selected_page_indices(document, page_ids)? {
        if trim.is_some_and(|trim| trim != document.pages[index].trim) {
            return Err(ModelError::MasterPageSizeMismatch.into());
        }
        document.pages[index].master_id = master_id.cloned();
    }
    Ok(())
}

pub(super) fn suppress_numbers(
    document: &mut Document,
    page_ids: &[crate::model::PageId],
    suppressed: bool,
) -> Result<(), CommandError> {
    for index in selected_page_indices(document, page_ids)? {
        document.pages[index].suppress_page_number = suppressed;
    }
    Ok(())
}

fn selected_page_indices(
    document: &Document,
    page_ids: &[crate::model::PageId],
) -> Result<Vec<usize>, CommandError> {
    if page_ids.is_empty()
        || page_ids.len() > usize::from(Document::MAX_INITIAL_PAGES)
        || page_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != page_ids.len()
    {
        return Err(CommandError::InvalidPageSelection);
    }
    page_ids.iter().map(|id| page_index(document, id)).collect()
}

pub(super) fn remove_master(
    document: &mut Document,
    id: &MasterId,
    replacement: Option<&MasterId>,
) -> Result<(), CommandError> {
    let index = master_index(document, id)?;
    if replacement == Some(id) {
        return Err(ModelError::MasterRemovalReplacementIsSame.into());
    }
    if let Some(replacement) = replacement {
        master_index(document, replacement)?;
    }
    let page_ids = document
        .pages
        .iter()
        .filter(|page| page.master_id.as_ref() == Some(id))
        .map(|page| page.id.clone())
        .collect::<Vec<_>>();
    if !page_ids.is_empty() {
        assign_master(document, &page_ids, replacement)?;
    }
    document.masters.remove(index);
    Ok(())
}

pub(super) fn transfer_deleted_section(document: &mut Document, index: usize) {
    if let Some(section) = document.pages[index].numbering_section
        && let Some(next) = document.pages.get_mut(index + 1)
        && next.numbering_section.is_none()
    {
        next.numbering_section = Some(section);
    }
}
