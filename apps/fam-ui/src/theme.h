#ifndef SHELL_THEME_H
#define SHELL_THEME_H

#include <QColor>
#include <QFont>
#include <QString>
#include <QVector>

/* The shell's design system: Ant Design 5's token set, in exactly one place.
 *
 * Nothing in this app paints with a literal colour. Widgets get their looks from
 * `Theme::styleSheet()` (Qt style sheets have no variables, so the tokens are substituted into the
 * sheet here rather than at each call site), and the one thing QSS cannot reach - the file row,
 * which is painted by hand - takes `QColor`s out of the same `Tokens`. Both halves are built from
 * the same struct, so a surface cannot drift into a different blue than the delegate does.
 *
 * Where the numbers come from is not decoration, it's the audit:
 *   - The light map is Ant Design's published default theme (`colorPrimary #1677ff`,
 *     `colorText rgba(0,0,0,0.88)`, `colorSplit rgba(5,5,5,0.06)`, `borderRadius 6`, `controlHeight
 *     32`, and so on).
 *   - The dark base surfaces (`#141414` container, `#000000` layout) are published; the text and
 *     fill ramps are Ant Design's algorithm (the same opacities over white instead of over black),
 *     and the two places where that algorithm would hand back something unreadable are decided
 *     explicitly rather than copied - see `Tokens::accentText` and `Tokens::primarySolid`.
 *   - `contrastReport()` recomputes the WCAG ratios of the pairs this theme actually renders, so a
 *     claim about readability in a note is a measurement the running program can repeat, not
 *     arithmetic done by whoever wrote it. */

enum class Scheme { Light, Dark };

/* Alpha-composite `layer` over an opaque `background` and return the flattened colour.
 *
 * Both halves of this theme need it: the style sheet cannot express "the hover fill, on whatever is
 * behind it", and a hand-painted row must not ask QPainter to blend over a background it then
 * changes. Anything that reports a contrast ratio has to go through here first, because a ratio
 * measured against a raw rgba() value describes no colour a person can see. */
QColor flattenOnto(const QColor &layer, const QColor &background);

/* Geometry and colour, in Ant Design's own names so a value can be checked against the upstream
 * table without a translation step. */
struct Tokens {
    Scheme scheme = Scheme::Light;

    /* Brand. Three blues, and the difference between them is legibility, not taste:
     *   `primary`      - the interaction colour where blue is a *fill or a border*.
     *   `accentText`   - the colour where blue is *text*. Ant Design's blue 6 carries white body text
     *                    at 4.10:1 and blue-6 text on white at the same 4.10:1, which is under the
     *                    4.5:1 this design checks itself against, so text takes blue 7 on light and
     *                    blue 5 on dark, both of which measure 6.2:1 and better.
     *   `primarySolid` - the fill of a button whose label is `onPrimary`, and its two states.
     * Measured, printed by `contrastReport()`, and no pair is allowed to be described as passing
     * when the arithmetic says otherwise. */
    QColor primary;
    QColor primaryHover;
    QColor primaryActive;
    QColor primaryBg;
    QColor primarySolid;
    QColor primarySolidHover;
    QColor primarySolidActive;
    QColor accentText;
    QColor onPrimary;
    /* The hairline of a tag chip in a row. The chip's ground is `primaryBg` and its text is
     * `accentText`, both of which exist already; the border is the one thing a preset Tag has that
     * this theme did not, and it is not optional here: `bgSelected` *is* `primaryBg`, so on a
     * selected row the chip's ground equals the row's and the outline is all that still says "this
     * was a tag". Ant Design's own Table does exactly that, and the light value is its published
     * `colorPrimaryBorder` (blue 3). */
    QColor tagBorder;
    /* The exclusion trio, for a filter chip whose name is being *excluded*: Ant Design's
     * colorError family. The text takes the darker red (the plain error red fails 4.5:1 as 12px
     * text on the bg); the ground and border are the published errorBg/errorBorder. */
    QColor errorText;
    QColor errorBg;
    QColor errorBorder;
    /* The inverse of the spotlight: Ant Design's dark tooltip is a *light* bubble, so the text on it
     * is dark, and hard-coding white here is how a tooltip becomes unreadable in exactly the scheme
     * where nobody thought to look. */
    QColor onSpotlight;

    /* Neutrals. */
    QColor text;
    QColor textSecondary;
    QColor textTertiary;
    QColor textQuaternary;
    QColor bgLayout;
    QColor bgContainer;
    QColor bgElevated;
    QColor bgHover;
    QColor bgSelected;
    QColor bgSpotlight;
    QColor border;
    QColor borderSecondary;
    QColor split;
    QColor fill;
    QColor fillSecondary;
    QColor fillTertiary;
    /* colorFillAlter: what a header strip and a hovered row are made of. `colorFillTertiary` at 4%
     * reads as nothing over a white container at 14px text, so the surface token is its own. */
    QColor fillAlter;
    QColor scrollbar;
    QColor scrollbarHover;

    /* Type scale, in CSS pixels: 12 / 14 / 16 / 20, with 22 as the 14px line box (lineHeight
     * 1.5714). */
    int fontSM = 12;
    int fontBase = 14;
    int fontLG = 16;
    int fontHeading = 20;
    int lineHeight = 22;

    /* Radius scale: 2 / 4 / 6 / 8. */
    int radiusXS = 2;
    int radiusSM = 4;
    int radius = 6;
    int radiusLG = 8;

    /* Control heights: 24 / 32 / 40, and the row a file list draws at - 40, which is `controlHeightLG`
     * rather than `controlHeight` because the row carries an icon and a tag as well as text. */
    int controlSM = 24;
    int control = 32;
    int controlLG = 40;
    int rowHeight = 40;

    /* The 4px spacing ramp (paddingXXS .. marginXL). */
    int xxs = 4;
    int xs = 8;
    int sm = 12;
    int md = 16;
    int lg = 24;

    /* One horizontal padding for text controls: Ant Design's inputs and default buttons sit content
     * 15px from the edge, which is `controlPaddingInline`. */
    int controlPaddingInline = 15;
};

class Theme {
public:
    explicit Theme(Scheme scheme = Scheme::Light);

    /* The one theme the shell paints with. Delegates hold a reference to it rather than a copy of its
     * colours, which is what lets a scheme switch take effect on a row that is already on screen
     * without every column being rebuilt. */
    static Theme &instance();

    /* Swaps the token set in place. The caller re-applies the style sheet afterwards - the theme
     * knows the numbers, the application knows which widgets exist. */
    void setScheme(Scheme scheme);

    const Tokens &tokens() const { return m_tokens; }
    Scheme scheme() const { return m_tokens.scheme; }
    QString name() const { return m_tokens.scheme == Scheme::Light ? QStringLiteral("light")
                                                                   : QStringLiteral("dark"); }

    /* The application-wide style sheet, with every token already substituted. */
    const QString &styleSheet() const { return m_sheet; }

    /* The UI font at the base size. Family order is Windows-first (Segoe UI Variable Text on 11,
     * Segoe UI on 10) with a CJK face named next, because this shell's users have Chinese file
     * names: leaving the fallback to the platform is how a list ends up rendering some rows in one
     * face and some in another. */
    QFont fontAt(int pixelSize, bool bold = false) const;
    QFont applicationFont() const { return fontAt(m_tokens.fontBase); }

    /* Every text-and-ground pair this theme actually paints, with its measured WCAG ratio. The
     * report and any caller that wants the single tightest number both read this one list, so a pair
     * cannot be added to one and missed by the other.
     *
     * `exempt` marks a pair that is measured and shown but not gated, because the standard excuses it
     * (a disabled control) or because keeping Ant Design's own number was the decision (a 1px input
     * border). An exempt pair still prints its ratio; nothing passes by being left out. */
    struct ContrastPair {
        QString label;
        double ratio;
        double floor;
        bool exempt;
        bool passes;
    };
    QVector<ContrastPair> contrastPairs() const;

    /* The report as text: one line per pair, then a count of how many gated pairs fell short. */
    QString contrastReport() const;

    /* The tightest gated pair, for a footer that has room for one number. Returns 99.0 when a theme
     * somehow has no gated pairs, so a missing measurement reads as a glaring pass rather than a
     * silent zero. */
    double tightestRatio() const;

    /* The scheme the desktop is asking for. Unknown counts as light: a shell that guessed dark on a
     * machine reporting "no preference" would look broken to the person who just set a white
     * wallpaper. */
    static Scheme systemScheme();
    static Scheme opposite(Scheme scheme) {
        return scheme == Scheme::Light ? Scheme::Dark : Scheme::Light;
    }
    static QString shortName(Scheme scheme) {
        return scheme == Scheme::Light ? QStringLiteral("light") : QStringLiteral("dark");
    }

    /* Widgets reach their variant through object names, so the style sheet stays declarative. One
     * list, so a call site cannot invent a name QSS has no rule for. */
    static const char *pageName() { return "page"; }
    static const char *toolbarName() { return "toolbar"; }
    static const char *toolbarActionsName() { return "toolbarActions"; }
    static const char *searchName() { return "searchInput"; }
    /* The note dialog's editor - a QTextEdit, which the sheet's QLineEdit rules do not reach. */
    static const char *noteEditorName() { return "noteEditor"; }
    static const char *primaryButtonName() { return "primaryButton"; }
    static const char *iconButtonName() { return "iconButton"; }
    /* A button that stays down - the split toggle. It needs its own rule because a control whose
     * state you cannot see is a control that appears not to work. */
    static const char *toggleButtonName() { return "toggleButton"; }
    /* The vocabulary strip under the toolbar, and the one chip on it: `tagFilterBar` owns the
     * surface, `tagChip` owns a chip - two names because the strip is a place and the chip is a
     * control a person acts on, and a style rule for one is not a rule for the other. */
    static const char *tagFilterBarName() { return "tagFilterBar"; }
    static const char *tagChipName() { return "tagChip"; }
    /* The menu a chip's right-click builds - a QMenu, which otherwise takes the desktop's colours
     * the same way an unnamed QDialog did. */
    static const char *tagChipMenuName() { return "tagChipMenu"; }
    /* The gear at the strip's far end - a fixed 32px square, outside the chips' scroll. */
    static const char *tagGearName() { return "tagGear"; }
    static const char *previewName() { return "previewPanel"; }
    static const char *previewHeaderName() { return "previewHeader"; }
    static const char *previewMetaName() { return "previewMeta"; }
    static const char *previewListName() { return "previewList"; }
    static const char *previewContentName() { return "previewContent"; }
    static const char *sortComboName() { return "sortCombo"; }
    static const char *columnPropName() { return "columnProp"; }
    static const char *propLabelName() { return "propLabel"; }
    static const char *sidebarName() { return "sidebar"; }
    static const char *columnFrameName() { return "column"; }
    static const char *columnHeaderName() { return "columnHeader"; }
    static const char *sortHeaderName() { return "sortHeader"; }
    static const char *pathName() { return "columnPath"; }
    static const char *listName() { return "fileList"; }
    static const char *statusName() { return "statusLine"; }
    static const char *emptyName() { return "emptyState"; }

private:
    QString buildSheet() const;

    Tokens m_tokens;
    QString m_sheet;
};

#endif /* SHELL_THEME_H */
