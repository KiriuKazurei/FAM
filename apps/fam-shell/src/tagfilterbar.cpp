#include "tagfilterbar.h"

#include <QHBoxLayout>
#include <QMenu>
#include <QPushButton>
#include <QScrollArea>

#include "bridge.h"
#include "theme.h"

TagFilterBar::TagFilterBar(FamBridge *bridge, QWidget *parent) : QWidget(parent), m_bridge(bridge) {
    setObjectName(QString::fromLatin1(Theme::tagFilterBarName()));
    /* A strip that scrolls rather than a strip that squeezes: a library of fifty tags must not turn
     * fifty chips into fifty slivers of a character each. The scrollbar only exists when it has
     * something to say, which keeps a small library visually identical to a bare row. */
    auto *scroll = new QScrollArea(this);
    scroll->setWidgetResizable(true);
    scroll->setFrameShape(QFrame::NoFrame);
    scroll->setHorizontalScrollBarPolicy(Qt::ScrollBarAsNeeded);
    scroll->setVerticalScrollBarPolicy(Qt::ScrollBarAlwaysOff);
    scroll->setStyleSheet(QStringLiteral("QScrollArea{background:transparent;}"));

    auto *content = new QWidget(scroll);
    content->setStyleSheet(QStringLiteral("background:transparent;"));
    m_chipsLayout = new QHBoxLayout(content);
    m_chipsLayout->setContentsMargins(0, 0, 0, 0);
    m_chipsLayout->setSpacing(Theme::instance().tokens().xxs);
    /* The stretch stays for the strip's whole life and keeps the chips left-aligned; a rebuild
     * inserts in front of it, which is why every chip goes in at `count() - 1`. */
    m_chipsLayout->addStretch(1);
    scroll->setWidget(content);

    auto *box = new QHBoxLayout(this);
    box->setContentsMargins(0, 0, 0, 0);
    box->addWidget(scroll);

    hide();
}

void TagFilterBar::clearChips() {
    for (QPushButton *chip : m_chips) {
        m_chipsLayout->removeWidget(chip);
        chip->deleteLater();
    }
    m_chips.clear();
}

void TagFilterBar::refresh(const QString &query) {
    clearChips();
    QString error;
    if (!m_bridge->tagVocabulary(&m_facets, &error) || m_facets.isEmpty()) {
        hide();
        return;
    }
    for (const TagFacet &facet : std::as_const(m_facets)) {
        auto *chip = new QPushButton(QStringLiteral("%1 · %2").arg(facet.tag).arg(facet.files),
                                     this);
        chip->setObjectName(QString::fromLatin1(Theme::tagChipName()));
        chip->setProperty("tagName", facet.tag);
        chip->setCheckable(true);
        chip->setCursor(Qt::PointingHandCursor);
        chip->setToolTip(QStringLiteral("%1 file(s) carry this tag · click to search it")
                             .arg(facet.files));
        connect(chip, &QPushButton::toggled, this, [this](bool) { emit filterChanged(); });
        /* The merge gesture lives on the chip's context menu: a click searches, a right-click
         * consolidates. The menu is built when it opens, from the facets this read brought back. */
        chip->setContextMenuPolicy(Qt::CustomContextMenu);
        connect(chip, &QWidget::customContextMenuRequested, this,
                [this, chip](const QPoint &at) {
                    m_menuTag = chip->property("tagName").toString();
                    chipMenu();
                });
        m_chipsLayout->insertWidget(m_chipsLayout->count() - 1, chip);
        m_chips.append(chip);
    }
    show();
    /* The rebuild is a view refresh, not a state change: what the query says stands. */
    applyQuery(query);
}

void TagFilterBar::chipMenu() {
    if (m_menuTag.isEmpty()) {
        return;
    }
    QMenu menu(this);
    menu.setObjectName(QString::fromLatin1(Theme::tagChipMenuName()));
    auto *title = menu.addAction(QStringLiteral("Merge '%1' into…").arg(m_menuTag));
    title->setEnabled(false);
    menu.addSeparator();
    bool others = false;
    for (const TagFacet &facet : std::as_const(m_facets)) {
        if (facet.tag == m_menuTag) {
            continue;
        }
        others = true;
        const QString target = facet.tag;
        connect(menu.addAction(QStringLiteral("%1 · %2").arg(facet.tag).arg(facet.files)),
                &QAction::triggered, this, [this, target] { emit mergeRequested(m_menuTag, target); });
    }
    if (!others) {
        menu.addAction(QStringLiteral("(no other tag in the library)"))->setEnabled(false);
    }
    menu.exec(QCursor::pos());
    m_menuTag.clear();
}

bool TagFilterBar::toggle(const QString &name) {
    for (QPushButton *chip : std::as_const(m_chips)) {
        if (chip->property("tagName").toString() == name) {
            chip->setChecked(!chip->isChecked());
            return true;
        }
    }
    return false;
}

bool TagFilterBar::markSilently(const QString &name, bool down) {
    for (QPushButton *chip : std::as_const(m_chips)) {
        if (chip->property("tagName").toString() == name) {
            if (chip->isChecked() != down) {
                chip->blockSignals(true);
                chip->setChecked(down);
                chip->blockSignals(false);
            }
            return true;
        }
    }
    return false;
}

void TagFilterBar::applyQuery(const QString &query) {
    const QStringList tokens = query.split(QChar(u' '), Qt::SkipEmptyParts);
    for (QPushButton *chip : std::as_const(m_chips)) {
        const QString wanted = QStringLiteral("tag:") + chip->property("tagName").toString();
        const bool marks = tokens.contains(wanted);
        if (chip->isChecked() != marks) {
            /* Silent, because the query is the thing that moved: the toggled signal is for a hand
             * on a chip, and this is the strip agreeing with a box - or with a rebuild - that it
             * itself is following. Letting it fire here would turn "rebuild and re-mark" into
             * "filterChanged → search → rebuild", and the run would chase its own tail. */
            chip->blockSignals(true);
            chip->setChecked(marks);
            chip->blockSignals(false);
        }
    }
}

QStringList TagFilterBar::checkedTags() const {
    QStringList names;
    for (const QPushButton *chip : std::as_const(m_chips)) {
        if (chip->isChecked()) {
            names.append(chip->property("tagName").toString());
        }
    }
    return names;
}

bool TagFilterBar::hasChip(const QString &name) const {
    for (const QPushButton *chip : std::as_const(m_chips)) {
        if (chip->property("tagName").toString() == name) {
            return true;
        }
    }
    return false;
}
