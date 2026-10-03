#include "tagfilterbar.h"

#include <QHBoxLayout>
#include <QMenu>
#include <QStyle>
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

void TagFilterBar::refresh(const QString &query, const QString &dir) {
    clearChips();
    QString error;
    const bool read = dir.isEmpty()
        ? m_bridge->tagVocabulary(&m_facets, &error)
        : m_bridge->tagVocabularyWithin(dir, &m_facets, &error);
    if (!read || m_facets.isEmpty()) {
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
        /* Left-click toggles include; right-click toggles exclude. The two states are mutually
         * exclusive by construction - including clears an exclusion, excluding clears an inclusion
         * - and each writes its own token into the box through filterChanged. */
        connect(chip, &QPushButton::clicked, this, [this, chip]() {
            const bool exclude = chip->property("chipState").toString()
                                 == QStringLiteral("exclude");
            applyChipState(chip, exclude ? State::Neutral : State::Include);
            emit filterChanged();
        });
        chip->setContextMenuPolicy(Qt::CustomContextMenu);
        connect(chip, &QWidget::customContextMenuRequested, this, [this, chip](const QPoint &) {
            const bool exclude = chip->property("chipState").toString()
                                 == QStringLiteral("exclude");
            applyChipState(chip, exclude ? State::Neutral : State::Exclude);
            emit filterChanged();
        });
        m_chipsLayout->insertWidget(m_chipsLayout->count() - 1, chip);
        m_chips.append(chip);
    }
    show();
    /* The rebuild is a view refresh, not a state change: what the query says stands. */
    applyQuery(query);
}

void TagFilterBar::applyChipState(QPushButton *chip, State state) {
    const QString name = QStringLiteral("%1").arg(static_cast<int>(state));
    if (chip->property("chipState").toString() == name) {
        return;
    }
    chip->setProperty("chipState", name);
    chip->setChecked(state == State::Include);
    /* The style sheet matches on properties, but only re-resolves a widget when told. */
    style()->unpolish(chip);
    style()->polish(chip);
    chip->update();
}

TagFilterBar::State TagFilterBar::stateOf(const QPushButton *chip) {
    const QString text = chip->property("chipState").toString();
    if (text == QStringLiteral("include")) {
        return State::Include;
    }
    if (text == QStringLiteral("exclude")) {
        return State::Exclude;
    }
    return State::Neutral;
}

bool TagFilterBar::toggle(const QString &name) {
    for (QPushButton *chip : std::as_const(m_chips)) {
        if (chip->property("tagName").toString() == name) {
            applyChipState(chip, stateOf(chip) == State::Include ? State::Neutral
                                                                 : State::Include);
            return true;
        }
    }
    return false;
}

bool TagFilterBar::setExcluded(const QString &name, bool excluded) {
    for (QPushButton *chip : std::as_const(m_chips)) {
        if (chip->property("tagName").toString() == name) {
            applyChipState(chip, excluded ? State::Exclude : State::Neutral);
            return true;
        }
    }
    return false;
}

void TagFilterBar::applyQuery(const QString &query) {
    const QStringList tokens = query.split(QChar(u' '), Qt::SkipEmptyParts);
    const QString includeMark = QStringLiteral("tag:");
    const QString excludeMark = QStringLiteral("tag:!");
    for (QPushButton *chip : std::as_const(m_chips)) {
        const QString name = chip->property("tagName").toString();
        State state = State::Neutral;
        if (tokens.contains(includeMark + name)) {
            state = State::Include;
        } else if (tokens.contains(excludeMark + name)) {
            state = State::Exclude;
        }
        /* Silent and signal-free: the query is the thing that moved, and the strip is agreeing
         * with a box - or with a rebuild - that it itself is following. Letting a click's signal
         * fire here would turn "rebuild and re-mark" into "filterChanged → search → rebuild",
         * and the run would chase its own tail. */
        applyChipState(chip, state);
    }
}

QStringList TagFilterBar::checkedTags() const {
    QStringList names;
    for (const QPushButton *chip : std::as_const(m_chips)) {
        if (chip->property("chipState").toString() == QStringLiteral("include")) {
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
